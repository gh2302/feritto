//! A single SQLite writer, bounded work queue, private payload storage, and literal search.
use anyhow::{Context, Result, bail};
use crossbeam_channel::{Sender, bounded};
use feritto_core::*;
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
    thread,
};

const SCHEMA_VERSION: i64 = 1;
const INLINE_LIMIT: usize = 1024 * 1024;
const SEARCH_LIMIT: usize = 64 * 1024;

#[derive(Clone)]
pub struct Store {
    tx: Sender<Job>,
}
type Job = Box<dyn FnOnce(&mut Repository) + Send>;
impl Store {
    pub fn open(directory: PathBuf) -> Result<Self> {
        let repo = Repository::open(directory)?;
        let (tx, rx) = bounded::<Job>(64);
        thread::Builder::new()
            .name("feritto-store".into())
            .spawn(move || {
                let mut repo = repo;
                while let Ok(job) = rx.recv() {
                    job(&mut repo);
                }
            })?;
        Ok(Self { tx })
    }
    pub fn call<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Repository) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (tx, rx) = bounded(1);
        self.tx
            .send(Box::new(move |repo| {
                let _ = tx.send(f(repo));
            }))
            .map_err(|_| anyhow::anyhow!("database worker stopped"))?;
        rx.recv().context("database worker stopped")?
    }
}

pub fn private_dir(path: &Path) -> Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("pending");
    let mut f = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&tmp)?;
    f.set_permissions(fs::Permissions::from_mode(0o600))?;
    f.write_all(bytes)?;
    f.sync_all()?;
    fs::rename(&tmp, path)?;
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

pub struct Repository {
    db: Connection,
    directory: PathBuf,
}
#[derive(Debug, Clone)]
pub struct Thumbnail {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Repository {
    pub fn open(directory: PathBuf) -> Result<Self> {
        private_dir(&directory)?;
        private_dir(&directory.join("payloads"))?;
        let path = directory.join("history.sqlite3");
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        let db = Connection::open(&path)?;
        db.busy_timeout(std::time::Duration::from_secs(3))?;
        let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            bail!(
                "history database uses newer schema {version}; this version supports {SCHEMA_VERSION}"
            );
        }
        db.execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA cache_size=-8192; PRAGMA secure_delete=ON;")?;
        if version == 0 {
            let existing: i64 = db.query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table'",
                [],
                |r| r.get(0),
            )?;
            if existing != 0 {
                bail!("unrecognized database; refusing to migrate it as native history");
            }
            db.execute_batch(include_str!("schema.sql"))?;
        }
        let repo = Self { db, directory };
        repo.recover_payloads()?;
        Ok(repo)
    }

    fn recover_payloads(&self) -> Result<()> {
        for entry in fs::read_dir(self.directory.join("payloads"))? {
            let path = entry?.path();
            let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            let used: bool = self.db.query_row(
                "SELECT EXISTS(SELECT 1 FROM representations WHERE payload=?1)",
                [name],
                |r| r.get(0),
            )?;
            if !used && path.is_file() {
                fs::remove_file(path)?;
            }
        }
        Ok(())
    }

    pub fn insert(&mut self, capture: &Capture, settings: &Settings) -> Result<ClipId> {
        let offer = &capture.offer;
        if offer.representations.is_empty() {
            bail!("empty clipboard offer");
        }
        let digest = offer.digest();
        let existing: Option<i64> = self
            .db
            .query_row(
                "SELECT id FROM clips WHERE content_hash=?1",
                [&digest],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = existing {
            self.db.execute(
                "UPDATE clips SET captured_at=?1 WHERE id=?2",
                params![now_ms(), id],
            )?;
            return Ok(id);
        }
        let text = offer.text_content().unwrap_or_else(|| {
            offer
                .representations
                .iter()
                .find(|r| r.target == "text/uri-list" || r.target == "text/html")
                .map(|r| String::from_utf8_lossy(&r.bytes).into_owned())
                .unwrap_or_default()
        });
        let normalized = normalize_search(&text);
        let search = bounded_text(&normalized, SEARCH_LIMIT);
        let truncated = search.len() < normalized.len();
        let has_image = offer
            .representations
            .iter()
            .any(|r| r.target == "image/png");
        let preview = if text.trim().is_empty() {
            if has_image {
                "Image".to_string()
            } else {
                offer
                    .representations
                    .iter()
                    .map(|r| r.target.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        } else {
            bounded_text(&text, 4096)
        };
        let thumbnail = offer
            .representations
            .iter()
            .find(|r| r.target == "image/png")
            .and_then(|r| thumbnail_png(&r.bytes).ok());
        // Publish durable payloads before the transaction references them. Startup collects crash orphans.
        let mut payloads = Vec::new();
        for r in &offer.representations {
            if r.bytes.len() > INLINE_LIMIT {
                let key = blake3::hash(&r.bytes).to_hex().to_string();
                let path = self.directory.join("payloads").join(&key);
                if !path.exists() {
                    write_private(&path, &r.bytes)?;
                }
                payloads.push(Some(key));
            } else {
                payloads.push(None);
            }
        }
        let tx = self.db.transaction()?;
        tx.execute("INSERT INTO clips(captured_at,content_hash,preview,search_text,search_truncated,size,source,complete,omitted,has_image) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![now_ms(),digest,preview,search,truncated,offer.size() as i64,serde_json::to_string(&capture.source)?,capture.complete,serde_json::to_string(&capture.omitted)?,has_image])?;
        let id = tx.last_insert_rowid();
        for (r, payload) in offer.representations.iter().zip(payloads) {
            let inline = if payload.is_none() {
                Some(r.bytes.as_ref())
            } else {
                None
            };
            tx.execute("INSERT INTO representations(clip_id,target,mime,inline,payload,size) VALUES(?1,?2,?3,?4,?5,?6)", params![id,r.target,r.mime,inline,payload,r.bytes.len() as i64])?;
        }
        if let Some(png) = thumbnail {
            tx.execute(
                "INSERT INTO assets(clip_id,kind,version,bytes) VALUES(?1,'thumbnail',1,?2)",
                params![id, png],
            )?;
        }
        tx.commit()?;
        self.cleanup(settings)?;
        Ok(id)
    }

    pub fn search_ids(&self, query: &str) -> Result<Vec<ClipId>> {
        let query = normalize_search(&bounded_text(query, 4096));
        let indexed = query.chars().count() >= 3 && !query.contains('\0');
        let sql = if query.is_empty() {
            "SELECT id FROM clips ORDER BY starred DESC,captured_at DESC,id DESC LIMIT 100000"
        } else if indexed {
            "SELECT c.id FROM clips c JOIN clips_fts f ON f.rowid=c.id WHERE clips_fts MATCH ?2 AND instr(c.search_text,?1)>0 ORDER BY c.starred DESC,c.captured_at DESC,c.id DESC LIMIT 100000"
        } else {
            "SELECT id FROM clips WHERE instr(search_text,?1)>0 ORDER BY starred DESC,captured_at DESC,id DESC LIMIT 100000"
        };
        let mut stmt = self.db.prepare(sql)?;
        let read = |r: &rusqlite::Row<'_>| r.get::<_, i64>(0);
        let ids = if query.is_empty() {
            stmt.query_map([], read)?.collect::<rusqlite::Result<_>>()?
        } else if indexed {
            let phrase = format!("\"{}\"", query.replace('"', "\"\""));
            stmt.query_map(params![query, phrase], read)?
                .collect::<rusqlite::Result<_>>()?
        } else {
            stmt.query_map([&query], read)?
                .collect::<rusqlite::Result<_>>()?
        };
        Ok(ids)
    }

    pub fn summaries(&self, ids: &[ClipId]) -> Result<Vec<ClipSummary>> {
        let mut stmt = self.db.prepare_cached("SELECT id,preview,captured_at,size,starred,search_truncated,has_image FROM clips WHERE id=?1")?;
        let mut format_stmt = self.db.prepare_cached(
            "SELECT target FROM representations WHERE clip_id=?1 ORDER BY target",
        )?;
        let mut results = Vec::with_capacity(ids.len());
        for id in ids {
            let summary = stmt
                .query_row([id], |r| {
                    Ok(ClipSummary {
                        id: r.get(0)?,
                        preview: r.get(1)?,
                        captured_at: r.get(2)?,
                        size: r.get(3)?,
                        starred: r.get(4)?,
                        search_truncated: r.get(5)?,
                        has_image: r.get(6)?,
                        formats: vec![],
                    })
                })
                .optional()?;
            if let Some(mut summary) = summary {
                summary.formats = format_stmt
                    .query_map([id], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                results.push(summary);
            }
        }
        Ok(results)
    }

    pub fn offer(&self, id: ClipId) -> Result<Arc<Offer>> {
        let mut stmt = self.db.prepare("SELECT target,mime,inline,payload FROM representations WHERE clip_id=?1 ORDER BY target")?;
        let mut representations = Vec::new();
        for row in stmt.query_map([id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<Vec<u8>>>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })? {
            let (target, mime, inline, payload) = row?;
            let bytes = if let Some(bytes) = inline {
                bytes
            } else {
                let key = payload.context("missing payload reference")?;
                if key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
                    bail!("invalid payload reference");
                }
                let bytes = fs::read(self.directory.join("payloads").join(&key))?;
                if blake3::hash(&bytes).to_hex().as_str() != key {
                    bail!("payload checksum mismatch");
                }
                bytes
            };
            representations.push(Representation {
                target,
                mime,
                bytes: Arc::from(bytes),
            });
        }
        if representations.is_empty() {
            bail!("clip no longer exists");
        }
        Ok(Arc::new(Offer { representations }))
    }

    pub fn thumbnail(&self, id: ClipId) -> Result<Option<Thumbnail>> {
        let bytes: Option<Vec<u8>> = self
            .db
            .query_row(
                "SELECT bytes FROM assets WHERE clip_id=?1 AND kind='thumbnail'",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        bytes
            .map(|b| {
                let image =
                    image::load_from_memory_with_format(&b, image::ImageFormat::Png)?.into_rgba8();
                Ok(Thumbnail {
                    width: image.width(),
                    height: image.height(),
                    rgba: image.into_raw(),
                })
            })
            .transpose()
    }
    pub fn toggle_star(&self, id: ClipId) -> Result<()> {
        self.db.execute("UPDATE clips SET starred=NOT starred,protection_at=CASE WHEN starred=0 THEN ?2 ELSE NULL END WHERE id=?1", params![id,now_ms()])?;
        Ok(())
    }
    pub fn mark_used(&self, id: ClipId) -> Result<()> {
        self.db.execute(
            "UPDATE clips SET last_used=?2 WHERE id=?1",
            params![id, now_ms()],
        )?;
        Ok(())
    }
    pub fn delete(&self, id: ClipId) -> Result<()> {
        self.db.execute("DELETE FROM clips WHERE id=?1", [id])?;
        self.recover_payloads()
    }
    pub fn clear_history(&self) -> Result<()> {
        self.db.execute("DELETE FROM clips", [])?;
        self.recover_payloads()
    }
    pub fn count(&self) -> Result<u64> {
        Ok(self
            .db
            .query_row("SELECT count(*) FROM clips", [], |r| r.get(0))?)
    }
    pub fn cleanup(&mut self, settings: &Settings) -> Result<()> {
        let tx = self.db.transaction()?;
        if settings.max_age_days > 0 {
            let cutoff = now_ms() - i64::from(settings.max_age_days) * 86400000;
            tx.execute("DELETE FROM clips WHERE captured_at<?1 AND starred=0 AND group_id IS NULL AND sticky_global IS NULL AND sticky_group IS NULL AND shortcut IS NULL", [cutoff])?;
        }
        let mut stmt = tx.prepare("SELECT id,size,starred OR group_id IS NOT NULL OR sticky_global IS NOT NULL OR sticky_group IS NOT NULL OR shortcut IS NOT NULL FROM clips ORDER BY captured_at DESC,id DESC")?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, u64>(1)?,
                    r.get::<_, bool>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        let mut bytes = 0u64;
        for (index, (id, size, protected)) in rows.into_iter().enumerate() {
            bytes = bytes.saturating_add(size);
            if !protected
                && (index >= settings.max_clips.max(1) || bytes > settings.max_history_bytes)
            {
                tx.execute("DELETE FROM clips WHERE id=?1", [id])?;
            }
        }
        tx.commit()?;
        self.recover_payloads()
    }
}

fn thumbnail_png(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut reader =
        image::ImageReader::with_format(std::io::Cursor::new(bytes), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode()?.thumbnail(320, 180);
    let mut output = std::io::Cursor::new(Vec::new());
    image.write_to(&mut output, image::ImageFormat::Png)?;
    Ok(output.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn capture(offer: Offer) -> Capture {
        Capture {
            generation: 1,
            offer: Arc::new(offer),
            source: SourceIdentity::default(),
            complete: true,
            omitted: vec![],
        }
    }
    #[test]
    fn roundtrip_search_retention_and_private_payloads() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut repo = Repository::open(dir.path().join("data"))?;
        let settings = Settings {
            max_clips: 2,
            ..Default::default()
        };
        let first = repo.insert(
            &capture(Offer::text("Ｓtraße café 100% _ \"quote\"")),
            &settings,
        )?;
        repo.toggle_star(first)?;
        for q in ["STRASSE", "cafe\u{301}", "100%", "_", "\"quote\""] {
            assert_eq!(repo.search_ids(q)?, vec![first]);
        }
        assert!(repo.search_ids("absent")?.is_empty());
        let mut large = Offer::text("binary");
        large.representations.push(Representation {
            target: "application/test".into(),
            mime: None,
            bytes: Arc::from(vec![42u8; INLINE_LIMIT + 10]),
        });
        let id = repo.insert(&capture(large.clone()), &settings)?;
        assert_eq!(*repo.offer(id)?, large);
        assert_eq!(repo.insert(&capture(large.clone()), &settings)?, id);
        let payload = fs::read_dir(dir.path().join("data/payloads"))?
            .next()
            .unwrap()?
            .path();
        assert_eq!(fs::metadata(&payload)?.permissions().mode() & 0o777, 0o600);
        assert_eq!(
            fs::metadata(dir.path().join("data"))?.permissions().mode() & 0o777,
            0o700
        );
        repo.insert(&capture(Offer::text("third")), &settings)?;
        assert!(!repo.search_ids("strasse")?.is_empty());
        repo.clear_history()?;
        assert_eq!(repo.count()?, 0);
        assert_eq!(fs::read_dir(dir.path().join("data/payloads"))?.count(), 0);
        assert_eq!(*Arc::new(large.clone()), large); // active immutable offers survive deletion
        Ok(())
    }
    #[test]
    fn newer_schema_is_refused() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let repo = Repository::open(dir.path().to_path_buf())?;
        repo.db.pragma_update(None, "user_version", 99)?;
        drop(repo);
        assert!(Repository::open(dir.path().to_path_buf()).is_err());
        Ok(())
    }
}
