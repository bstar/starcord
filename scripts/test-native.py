#!/usr/bin/env python3
"""Check Focus Signal in a private Kitty replay; no account or real session."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import signal
import shutil
import subprocess
import tempfile
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary', required=True)
parser.add_argument('--kitty', default='kitty')
parser.add_argument('--kitten', default='kitten')
parser.add_argument('--output', required=True)
args = parser.parse_args()
output = Path(args.output).resolve()
output.mkdir(parents=True, exist_ok=True)
fixture = Path(__file__).resolve().parent.parent / 'testdata/gateway/session.json'

with tempfile.TemporaryDirectory(prefix='cord-native-',dir='/tmp') as temporary:
    private = Path(temporary)
    runtime = private / 'runtime'
    runtime.mkdir(mode=0o700)
    configuration=private / 'config'
    configuration.mkdir(mode=0o700)
    (configuration / 'config.toml').write_text('[ui]\ntheme = "kanagawa"\n[media]\nanimate = "never"\n')
    # Recovery needs a stable scene: omit later synthetic messages, typing and
    # reconnects, which are covered by the core tests and interactive replay.
    timeline=json.loads(fixture.read_text())
    timeline['steps']=[step for step in timeline['steps'] if step.get('at_ms',0)<=1000]
    timeline['media']={key:str((fixture.parent / path).resolve()) for key,path in timeline.get('media',{}).items()}
    if timeline.get('messages'):
        timeline['messages']=str((fixture.parent / timeline['messages']).resolve())
    stable_fixture=private / 'session.json'
    stable_fixture.write_text(json.dumps(timeline))
    original_runtime = os.environ.get('XDG_RUNTIME_DIR', f'/run/user/{os.getuid()}')
    environment = {
        **os.environ, 'XDG_RUNTIME_DIR': str(runtime),
        'WAYLAND_DISPLAY': str(Path(original_runtime) / os.environ.get('WAYLAND_DISPLAY', 'wayland-1')),
        'STARCORD_DIR': str(private / 'data'), 'STARCORD_CONFIG_DIR': str(private / 'config'),
        'STAR_GRAPHICS_SYSTEM_FONTS': '0',
        'STARCORD_LOG': 'starcord=debug,starkit=debug',
        'DBUS_SESSION_BUS_ADDRESS': 'unix:path=/nonexistent/starcord-native-test-bus',
    }
    fault = private / 'discard_images.py'
    fault.write_text('''from kittens.tui.handler import result_handler
def main(args): pass
@result_handler(no_ui=True)
def handle_result(args, result, target_window_id, boss):
    boss.window_id_map[target_window_id].screen.erase_in_display(2, False)
''')
    address = f'unix:{private}/kitty.sock'
    log = (output / 'kitty.log').open('w')
    child = subprocess.Popen([
        args.kitty, '--config', '/dev/null', '--listen-on', address,
        '-o', 'allow_remote_control=yes', '-o', 'linux_display_server=wayland',
        '-o', 'background=#1f1f28', '--title', 'STARCORD-FOCUS-SIGNAL-PROOF',
        str(Path(args.binary).resolve()), '--replay', str(stable_fixture),
    ], env=environment, stdout=log, stderr=log)

    def remote(*command):
        try:
            return subprocess.check_output(
                [args.kitten, '@', '--to', address, *command],
                stderr=subprocess.PIPE, timeout=10,
            )
        except subprocess.CalledProcessError as error:
            raise RuntimeError(error.stderr.decode()) from error

    def key(name):
        remote('send-key', '--match', 'id:1', name)

    def zero_mouse_coordinates():
        # Terminal edge reports can contain zero coordinates. Crossterm 0.29.0
        # panicked while converting them to zero-based positions, including
        # before the graphical frontend finished starting. Exercise SGR,
        # legacy and rxvt motion reports without clicking any application UI.
        for report in ('\x1b[<35;0;0M', '\x1b[MC  ', '\x1b[67;0;0M'):
            remote('send-text', '--match', 'id:1', '--', report)
            time.sleep(.1)

    def screenshot(name):
        assert child.poll() is None, f'Private frontend exited ({child.returncode}); inspect app.log and kitty.log'
        time.sleep(.3)
        path = output / f'{name}.png'
        remote('screenshot', '--match', 'id:1', str(path))
        return hashlib.sha256(path.read_bytes()).hexdigest()

    try:
        deadline = time.monotonic() + 20
        while True:
            try:
                remote('ls')
                break
            except (subprocess.SubprocessError, RuntimeError):
                assert child.poll() is None and time.monotonic() < deadline, 'Kitty failed to start'
                time.sleep(.1)
        time.sleep(2)
        zero_mouse_coordinates()
        screenshot('replay-login')
        # The replay deliberately exercises a simulated scan and confirmation
        # before READY. No phone or real login is involved.
        time.sleep(6)
        initial = screenshot('initial')
        key('ctrl+k')
        remote('send-text', '--match', 'id:1', '--', 'general')
        time.sleep(.5)
        key('enter')
        key('alt+3')
        conversation = screenshot('conversation')
        assert conversation != initial, 'Channel navigation did not change the native UI'
        key('f1')
        modal = screenshot('help')
        assert modal != conversation, 'Help did not open'
        key('escape')
        key('alt+4')
        remote('send-text', '--match', 'id:1', '--', 'Focus Signal draft')
        screenshot('draft-native')
        key('f9')
        zero_mouse_coordinates()
        screenshot('draft-cells')
        text = remote('get-text', '--match', 'id:1').decode()
        assert 'Focus Signal draft' in text, 'Draft was lost when switching to cells'
        key('f9')
        key('alt+3')
        reference = screenshot('restored-native')
        key('ctrl+l')
        assert screenshot('redraw') == reference, 'Ctrl+L changed the native scene'
        remote('kitten', '--match', 'id:1', str(fault))
        deadline = time.monotonic() + 6
        while screenshot('recovered') != reference:
            assert time.monotonic() < deadline, 'Missing native images did not recover'
            time.sleep(.2)
        key('ctrl+c')
        screenshot('quit-with-draft')
        key('y')
        child.wait(timeout=15)
        assert child.returncode == 0, 'Private frontend did not exit cleanly'
        result = {'zero_mouse_coordinates': True,
                  'navigation': True, 'modal': True, 'draft_across_f9': True,
                  'redraw': True, 'image_recovery': True, 'clean_exit': True}
        (output / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
        print(json.dumps(result))
    finally:
        application_log=private / 'data/cache/starcord.log'
        if application_log.exists():
            shutil.copyfile(application_log,output / 'app.log')
        if child.poll() is None:
            child.terminate()
            child.wait(timeout=10)
        # Only the controller carrying this test's unique runtime marker.
        marker = f'XDG_RUNTIME_DIR={runtime}'.encode() + b'\0'
        for process in Path('/proc').iterdir():
            if not process.name.isdigit():
                continue
            try:
                if b'--serve' in (process / 'cmdline').read_bytes() and marker in (process / 'environ').read_bytes():
                    os.kill(int(process.name), signal.SIGTERM)
            except (OSError, PermissionError):
                pass
        log.close()
