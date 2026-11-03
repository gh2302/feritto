#!/usr/bin/env python3
"""End-to-end desktop checks on a private headless sway. Invoke through scripts/check-wayland.sh."""
import json
import os
import pathlib
import struct
import subprocess as sp
import sys
import tempfile
import time
import zlib

ROOT = pathlib.Path(__file__).resolve().parents[1]
BINARY = pathlib.Path(os.environ.get('FERITTO_BINARY', ROOT / 'target/debug/feritto'))
POPUP = 'io.github.gh2302.Feritto'
TARGET = 'feritto-paste-target'


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


def nodes(node):
    yield node
    for child in node.get('nodes', []) + node.get('floating_nodes', []):
        yield from nodes(child)


def windows():
    return [n for n in nodes(json.loads(run('swaymsg', '-t', 'get_tree'))) if n.get('app_id')]


def focused():
    return next((n['app_id'] for n in windows() if n.get('focused')), None)


def window(app_id, title=None):
    return next((n for n in windows() if n['app_id'] == app_id and (title is None or n.get('name') == title)), None)


def key(*names):
    for name in names:
        run('wtype', '-k', name)


def paste(*args):
    result = sp.run(['wl-paste', '--no-newline', *args], capture_output=True)
    return result.stdout if result.returncode == 0 else None


def main():
    assert os.environ.get('WAYLAND_DISPLAY'), 'run through scripts/check-wayland.sh'
    with tempfile.TemporaryDirectory(prefix='feritto-wayland-smoke-') as directory:
        base = pathlib.Path(directory)
        for name, variable in [('data', 'XDG_DATA_HOME'), ('config', 'XDG_CONFIG_HOME')]:
            path = base / name
            path.mkdir(mode=0o700)
            os.environ[variable] = str(path)
        runtime = pathlib.Path(os.environ['XDG_RUNTIME_DIR'])
        socket = runtime / 'feritto/service.sock'
        config = base / 'config/feritto'
        config.mkdir(mode=0o700)
        # foot pastes CLIPBOARD with Ctrl+Shift+V.
        (config / 'settings.json').write_text(json.dumps({'paste_shortcut': 'ControlShiftV'}))
        (config / 'settings.json').chmod(0o600)
        app_log = open(base / 'app.log', 'wb')
        app = terminal = source = None
        try:
            app = sp.Popen([str(BINARY), '--daemon'], stdout=app_log, stderr=sp.STDOUT)

            def cli(*args):
                return json.loads(run(BINARY, *args))

            wait_for(socket.exists, 'single-instance endpoint')
            status = wait_for(lambda: cli('--status'), 'service startup')
            caps = status['capabilities']
            assert caps['backend'].startswith('Wayland ('), caps
            for name in ['monitor', 'ownership', 'automatic_paste', 'source_identity', 'persistence']:
                assert caps[name] == 'Available', (name, caps)
            # Without a portal the hotkey degrades to a reason, never a silent wait.
            hotkey = wait_for(lambda: 'Unavailable' in (h := cli('--status')['capabilities']['global_hotkey']) and h, 'portal fallback')
            assert 'feritto --toggle' in hotkey['Unavailable'], hotkey

            first = cli('--add', 'searchable original clip')['id']
            cli('--add', 'another clipboard entry')
            assert [c['id'] for c in cli('--list', '--search', 'ORIGINAL')] == [first]

            terminal = sp.Popen(['foot', '--app-id', TARGET, 'sh', '-c', 'stty -echo; cat > "$XDG_DATA_HOME/pasted.txt"'], stdout=sp.DEVNULL, stderr=sp.DEVNULL)
            wait_for(lambda: focused() == TARGET, 'paste target focus')

            cli('--toggle')
            wait_for(lambda: focused() == POPUP, 'popup focus')
            popup = window(POPUP)
            assert popup['type'] == 'floating_con', popup['type']
            rect = popup['rect']
            assert 0 <= rect['x'] <= 1280 - rect['width'] and 0 <= rect['y'] <= 1024 - rect['height'], rect

            # Type into iced's search field, choose the only match, and paste it into foot.
            run('wtype', '-d', '5', 'original')
            time.sleep(0.3)
            key('Return')
            wait_for(lambda: focused() == TARGET and window(POPUP) is None, 'focus returned to target')
            pasted = base / 'data/pasted.txt'
            time.sleep(0.3)
            key('Return')  # Flush cat's line without running a shell command.
            wait_for(lambda: pasted.exists() and pasted.read_text() == 'searchable original clip\n', 'paste reached the intended client')
            assert paste() == b'searchable original clip'

            # The GUI has no Wayland clipboard; Ctrl+V in search pastes through the service.
            cli('--add', 'haystack with needle inside')
            source = sp.Popen(['wl-copy', '--foreground', 'needle'], stdout=sp.DEVNULL, stderr=sp.DEVNULL)
            wait_for(lambda: len(cli('--list', '--search', 'needle')) == 2, 'needle capture')
            cli('--add', 'unrelated newest entry')
            cli('--toggle')
            wait_for(lambda: focused() == POPUP, 'popup focus')
            run('wtype', '-M', 'ctrl', 'v', '-m', 'ctrl')
            time.sleep(0.3)
            key('Return')
            wait_for(lambda: focused() == TARGET and window(POPUP) is None, 'focus returned after search paste')
            time.sleep(0.3)
            key('Return')
            wait_for(lambda: pasted.read_text().endswith('\nneedle\n'), 'search field pasted the clipboard')
            source.terminate(); source.wait(); source = None

            # Escape closes the popup and returns focus; losing focus also closes it.
            durations = []
            for _ in range(int(os.environ.get('FERITTO_CYCLES', '20'))):
                start = time.monotonic()
                cli('--toggle')
                wait_for(lambda: focused() == POPUP, 'popup focus')
                durations.append((time.monotonic() - start) * 1000)
                key('Escape')
                wait_for(lambda: focused() == TARGET and window(POPUP) is None, 'Escape focus restoration')
            cli('--show')
            wait_for(lambda: focused() == POPUP, 'popup focus')
            time.sleep(0.3)
            run('swaymsg', f'[app_id="{TARGET}"] focus')
            wait_for(lambda: window(POPUP) is None, 'popup closes when it loses focus')

            # External copies are captured while hidden; the clipboard survives its source.
            source = sp.Popen(['wl-copy', '--foreground', 'external source exit fixture'], stdout=sp.DEVNULL, stderr=sp.DEVNULL)
            clip = wait_for(lambda: cli('--list', '--search', 'external source exit fixture'), 'external capture')[0]
            source.terminate(); source.wait(); source = None
            wait_for(lambda: paste() == b'external source exit fixture', 'source exit recovery')

            # Capture, thumbnail, and re-serve a PNG without altering its bytes.
            def chunk(kind, data):
                return struct.pack('!I', len(data)) + kind + data + struct.pack('!I', zlib.crc32(kind + data))
            png = b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('!2I5B', 32, 32, 8, 2, 0, 0, 0))
            png += chunk(b'IDAT', zlib.compress((b'\0' + bytes([70, 120, 210]) * 32) * 32)) + chunk(b'IEND', b'')
            source = sp.Popen(['wl-copy', '--foreground', '--type', 'image/png'], stdin=sp.PIPE, stdout=sp.DEVNULL, stderr=sp.DEVNULL)
            source.stdin.write(png); source.stdin.close()
            image = wait_for(lambda: next((c for c in cli('--list') if c['has_image']), None), 'image capture')
            source.terminate(); source.wait(); source = None
            cli('--copy', str(clip['id']))
            wait_for(lambda: paste() == b'external source exit fixture', 'history copy')
            cli('--copy', str(image['id']))
            wait_for(lambda: paste('--type', 'image/png') == png, 'PNG served unchanged')
            cli('--show')
            wait_for(lambda: focused() == POPUP, 'image popup')
            time.sleep(0.2)
            key('Escape')
            wait_for(lambda: window(POPUP) is None, 'image popup closed')

            # Closing settings must not quit the service.
            cli('--settings')
            wait_for(lambda: window(POPUP, 'feritto — Settings'), 'settings window')
            run('swaymsg', f'[app_id="{POPUP}"] focus')
            key('Escape')
            wait_for(lambda: window(POPUP, 'feritto — Settings') is None, 'settings closed')
            assert cli('--status')['running']

            cli('--pause'); wait_for(lambda: cli('--status')['paused'], 'pause')
            cli('--pause'); wait_for(lambda: not cli('--status')['paused'], 'resume')
            cli('--clear-clipboard')
            wait_for(lambda: paste() is None, 'clear clipboard')
            time.sleep(0.3)
            assert paste() is None, 'an explicit clear was resurrected'
            cli('--clear-history')
            assert cli('--status')['clips'] == 0
            for path in base.rglob('*'):
                if path.is_file() and 'feritto' in path.parts:
                    assert path.stat().st_mode & 0o077 == 0, str(path)
            assert socket.stat().st_mode & 0o077 == 0
            before = cli('--add', 'restart durability fixture')['id']
            cli('--quit'); app.wait(timeout=8)
            assert app.returncode == 0
            app = sp.Popen([str(BINARY), '--daemon'], stdout=app_log, stderr=sp.STDOUT)
            wait_for(lambda: socket.exists() and cli('--list', '--search', 'restart durability fixture'), 'persistent history')
            assert cli('--list', '--search', 'restart durability fixture')[0]['id'] == before
            cli('--quit'); app.wait(timeout=8)
            p95 = sorted(durations)[min(len(durations) - 1, int(len(durations) * 0.95))]
            print(json.dumps({'compositor': 'sway (headless)', 'cycles': len(durations), 'popup_focus_p95_ms': round(p95, 1),
                              'checks': 'capabilities, portal fallback, capture, search, verified paste, search-field paste, focus, source exit, PNG, settings, pause, clear, privacy, restart'}, indent=2))
        except BaseException:
            app_log.flush()
            print((base / 'app.log').read_text(errors='replace'), file=sys.stderr)
            raise
        finally:
            for process in [source, terminal, app]:
                if process is not None and process.poll() is None:
                    process.terminate()
                    try:
                        process.wait(timeout=3)
                    except sp.TimeoutExpired:
                        process.kill(); process.wait()


if __name__ == '__main__':
    main()
