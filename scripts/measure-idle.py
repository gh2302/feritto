#!/usr/bin/env python3
"""Measure a hidden service in an isolated X11 session (use xvfb-run + dbus-run-session)."""
import argparse
import json
import os
import pathlib
import struct
import subprocess as sp
import tempfile
import time
import zlib


def main():
    # The caller supplies an isolated native X11 server.
    os.environ.pop('WAYLAND_DISPLAY', None)
    os.environ['XDG_SESSION_TYPE'] = 'x11'
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=pathlib.Path, default=pathlib.Path('target/release/feritto'))
    parser.add_argument('--seconds', type=int, default=300)
    parser.add_argument('--output', type=pathlib.Path)
    args = parser.parse_args()
    binary = args.binary.resolve()
    with tempfile.TemporaryDirectory(prefix='feritto-idle-') as directory:
        base = pathlib.Path(directory)
        for key, name in [('XDG_DATA_HOME', 'data'), ('XDG_CONFIG_HOME', 'config'), ('XDG_RUNTIME_DIR', 'runtime')]:
            path = base / name
            path.mkdir(mode=0o700)
            os.environ[key] = str(path)
        log = open(base / 'service.log', 'wb')
        app = sp.Popen([str(binary), '--daemon'], stdout=log, stderr=sp.STDOUT)
        try:
            deadline = time.monotonic() + 8
            while not (base / 'runtime/feritto/service.sock').exists():
                if time.monotonic() > deadline or app.poll() is not None:
                    raise RuntimeError('Service did not start')
                time.sleep(0.02)
            def cli(*arguments):
                return json.loads(sp.check_output([str(binary), *arguments]))
            for index in range(20):
                cli('--add', f'Idle corpus clip {index} — café 世界')
            def chunk(kind, data):
                return struct.pack('!I', len(data)) + kind + data + struct.pack('!I', zlib.crc32(kind + data))
            for count, size in enumerate([32, 64, 128], 1):
                png = b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('!2I5B', size, size, 8, 2, 0, 0, 0))
                png += chunk(b'IDAT', zlib.compress((b'\0' + bytes([50, 100, 200]) * size) * size)) + chunk(b'IEND', b'')
                source = sp.Popen(['xclip', '-selection', 'clipboard', '-target', 'image/png', '-i', '-quiet'], stdin=sp.PIPE, stdout=sp.DEVNULL, stderr=sp.DEVNULL)
                try:
                    source.stdin.write(png); source.stdin.close()
                    deadline = time.monotonic() + 5
                    while sum(c['has_image'] for c in cli('--list')) < count:
                        if time.monotonic() > deadline:
                            raise RuntimeError('Image capture timed out')
                        time.sleep(0.02)
                finally:
                    source.terminate(); source.wait()
            time.sleep(1)
            def sample():
                fields = pathlib.Path(f'/proc/{app.pid}/stat').read_text().split()
                return int(fields[13]) + int(fields[14]), int(fields[23]) * os.sysconf('SC_PAGE_SIZE')
            start = time.monotonic()
            ticks, rss = sample()
            peak = rss
            while time.monotonic() - start < max(1, args.seconds):
                time.sleep(1)
                if app.poll() is not None:
                    raise RuntimeError('Service stopped during measurement')
                peak = max(peak, sample()[1])
            end_ticks, end_rss = sample()
            elapsed = time.monotonic() - start
            result = {
                'elapsed_seconds': round(elapsed, 1),
                'cpu_percent_one_core': round((end_ticks - ticks) / os.sysconf('SC_CLK_TCK') / elapsed * 100, 3),
                'rss_mib': round(end_rss / 1048576, 2), 'peak_rss_mib': round(peak / 1048576, 2),
                'corpus': '20 short Unicode text clips and 3 generated PNG clips with thumbnails',
                'display': 'Xvfb 1280x1024x24, hidden popup, no WM or tray watcher',
            }
            output = json.dumps(result, indent=2) + '\n'
            if args.output:
                args.output.write_text(output)
            print(output, end='')
        finally:
            if app.poll() is None:
                sp.run([str(binary), '--quit'], stdout=sp.DEVNULL)
                app.wait(timeout=8)
            log.close()


if __name__ == '__main__':
    main()
