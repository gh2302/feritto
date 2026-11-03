#!/usr/bin/env python3
"""Repeatable desktop checks. Invoke through scripts/check-x11.sh, on a private X server."""
import json
import os
import pathlib
import subprocess as sp
import sys
import tempfile
import time

ROOT = pathlib.Path(__file__).resolve().parents[1]
BINARY = pathlib.Path(os.environ.get('FERITTO_BINARY', ROOT / 'target/debug/feritto'))

def run(*args, **kwargs):
    return sp.run([str(a) for a in args], check=True, capture_output=True, **kwargs).stdout

def wait_for(predicate, label, seconds=8):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        try:
            result = predicate()
        except sp.CalledProcessError:
            result = False
        if result:
            return result
        time.sleep(0.02)
    raise AssertionError('Timed out: ' + label)

def main():
    wm_name = sys.argv[1] if len(sys.argv) > 1 else 'i3'
    with tempfile.TemporaryDirectory(prefix='feritto-smoke-') as directory:
        base = pathlib.Path(directory)
        for name, key in [('data', 'XDG_DATA_HOME'), ('config', 'XDG_CONFIG_HOME'), ('runtime', 'XDG_RUNTIME_DIR')]:
            path = base / name
            path.mkdir(mode=0o700)
            os.environ[key] = str(path)
        wm_log = open(base / 'wm.log', 'wb')
        (base / 'i3.conf').write_text('font pango:DejaVu Sans Mono 10\n')
        wm_args = ['i3', '-c', str(base / 'i3.conf')] if wm_name == 'i3' else ['xfwm4', '--compositor=off']
        wm = sp.Popen(wm_args, stdout=wm_log, stderr=sp.STDOUT)
        app_log = open(base / 'app.log', 'wb')
        app = None
        terminal = None
        source = None
        try:
            wait_for(lambda: b'WINDOW' in run('xprop', '-root', '_NET_SUPPORTING_WM_CHECK'), 'window manager')
            # xterm uses PRIMARY with Shift+Insert, selected explicitly in this profile.
            config = base / 'config/feritto'
            config.mkdir(mode=0o700)
            (config / 'settings.json').write_text(json.dumps({'paste_shortcut': 'ShiftInsert'}))
            (config / 'settings.json').chmod(0o600)
            app = sp.Popen([str(BINARY), '--daemon'], stdout=app_log, stderr=sp.STDOUT)
            def cli(*args):
                return json.loads(run(BINARY, *args))
            wait_for(lambda: (base / 'runtime/feritto/service.sock').exists(), 'single-instance endpoint')
            wait_for(lambda: cli('--status')['running'], 'service startup')
            assert cli('--status')['capabilities']['backend'] == 'X11'
            first = cli('--add', 'searchable original clip')['id']
            second = cli('--add', 'another clipboard entry')['id']
            assert [c['id'] for c in cli('--list', '--search', 'ORIGINAL')] == [first]
            assert app.poll() is None
            terminal = sp.Popen(['xterm', '-fa', 'DejaVu Sans Mono', '-title', 'feritto-paste-target', '-e', 'sh', '-c', 'stty -echo; cat > "$XDG_DATA_HOME/pasted.txt"'], stdout=sp.DEVNULL, stderr=sp.DEVNULL)
            target = wait_for(lambda: run('xdotool', 'search', '--onlyvisible', '--name', '^feritto-paste-target$').decode().strip() if terminal.poll() is None else None, 'paste target')
            run('xdotool', 'windowactivate', '--sync', target)
            cli('--show')
            popup = wait_for(lambda: run('xdotool', 'search', '--onlyvisible', '--name', '^feritto$').decode().strip(), 'visible popup')
            wait_for(lambda: run('xdotool', 'getwindowfocus').decode().strip() == popup, 'popup focus')
            hints = run('xprop', '-id', popup, '_NET_WM_STATE', '_NET_WM_WINDOW_TYPE')
            assert b'SKIP_TASKBAR' in hints and b'UTILITY' in hints
            # Type into iced's search field, then choose the only match and paste it.
            run('xdotool', 'type', '--clearmodifiers', '--delay', '5', 'original')
            time.sleep(0.25)
            run('xdotool', 'key', 'Return')
            wait_for(lambda: run('xdotool', 'getwindowfocus').decode().strip() == target, 'focus returned to target')
            # Flush the cat reader's current line without executing it in a shell.
            time.sleep(0.25)
            run('xdotool', 'key', 'Return')
            pasted = base / 'data/pasted.txt'
            wait_for(lambda: pasted.exists() and pasted.read_text() == 'searchable original clip\n', 'paste reached the intended client')
            assert run('xclip', '-selection', 'clipboard', '-o') == b'searchable original clip'
            # Closing settings must not quit the clipboard daemon.
            cli('--settings')
            settings = wait_for(lambda: run('xdotool', 'search', '--onlyvisible', '--name', 'feritto.*Settings').decode().strip(), 'settings window')
            run('xdotool', 'windowactivate', '--sync', settings)
            run('xdotool', 'key', 'Escape')
            time.sleep(0.1)
            assert cli('--status')['running']
            # Reuse one popup 100 times. Check focus and hiding on every cycle.
            durations = []
            for _ in range(int(os.environ.get('FERITTO_CYCLES', '100'))):
                run('xdotool', 'windowactivate', '--sync', target)
                start = time.monotonic()
                cli('--show')
                wait_for(lambda: run('xdotool', 'getwindowfocus').decode().strip() == popup, 'warm popup focus')
                durations.append((time.monotonic() - start) * 1000)
                run('xdotool', 'key', 'Escape')
                wait_for(lambda: run('xdotool', 'getwindowfocus').decode().strip() == target, 'Escape focus restoration')
            # An external copy is captured while the popup is hidden; source exit retains it.
            source = sp.Popen(['xclip', '-selection', 'clipboard', '-i', '-quiet'], stdin=sp.PIPE, stdout=sp.DEVNULL, stderr=sp.DEVNULL)
            source.stdin.write(b'external source exit fixture')
            source.stdin.close()
            wait_for(lambda: cli('--list', '--search', 'external source exit fixture'), 'external capture')
            source.terminate(); source.wait(); source = None
            wait_for(lambda: run('xclip', '-selection', 'clipboard', '-o') == b'external source exit fixture', 'source exit recovery')
            cli('--pause'); wait_for(lambda: cli('--status')['paused'], 'pause')
            cli('--pause'); wait_for(lambda: not cli('--status')['paused'], 'resume')
            cli('--clear-history')
            assert cli('--status')['clips'] == 0
            assert run('xclip', '-selection', 'clipboard', '-o') == b'external source exit fixture'
            # Files created by SQLite and the settings writer must stay private.
            for path in base.rglob('*'):
                if path.is_file() and ('feritto' in path.parts or path.name == 'service.sock'):
                    assert path.stat().st_mode & 0o077 == 0, str(path)
            before = cli('--add', 'restart durability fixture')['id']
            cli('--quit'); app.wait(timeout=8)
            assert app.returncode == 0
            app = sp.Popen([str(BINARY), '--daemon'], stdout=app_log, stderr=sp.STDOUT)
            wait_for(lambda: (base / 'runtime/feritto/service.sock').exists(), 'restart endpoint')
            wait_for(lambda: cli('--list', '--search', 'restart durability fixture'), 'persistent history')
            assert cli('--list', '--search', 'restart durability fixture')[0]['id'] == before
            cli('--quit'); app.wait(timeout=8)
            p95 = sorted(durations)[min(len(durations)-1, int(len(durations)*0.95))]
            print(json.dumps({'wm': wm_name, 'cycles': len(durations), 'warm_focus_p95_ms': round(p95, 1), 'checks': 'capture, search, paste, PRIMARY, focus, hints, settings, source exit, pause, privacy, retention, restart'}, indent=2))
        except BaseException:
            app_log.flush(); wm_log.flush()
            print((base / 'app.log').read_text(errors='replace'), file=sys.stderr)
            print((base / 'wm.log').read_text(errors='replace')[-3000:], file=sys.stderr)
            raise
        finally:
            for process in [source, terminal, app, wm]:
                if process is not None and process.poll() is None:
                    process.terminate()
                    try:
                        process.wait(timeout=3)
                    except sp.TimeoutExpired:
                        process.kill(); process.wait()

if __name__ == '__main__':
    main()
