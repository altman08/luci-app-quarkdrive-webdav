"""Run the actual init functions with simulated UCI/procd and real exit waits."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import unittest


INIT = Path(__file__).resolve().parents[1] / 'luci-app-quarkdrive-webdav/root/etc/init.d/quarkdrive-webdav'
HARNESS = r'''
extra_command() { :; }
logger() { :; }
config_load() { :; }
config_get() {
    local qd_config_value
    qd_config_value="$(printenv "QD_TEST_$3")" || qd_config_value="${4-}"
    eval "$1=\"\$qd_config_value\""
}
config_get_bool() { config_get "$@"; }
procd_open_instance() {
    QD_INSTANCE_OPENED=1
    printf 'instance\n' >> "$QD_TEST_DIR/events"
}
procd_set_param() { :; }
procd_append_param() { :; }
procd_close_instance() { :; }
procd_add_reload_trigger() { printf 'trigger\n' >> "$QD_TEST_DIR/events"; }
set_running() {
    printf '{"quarkdrive-webdav":{"instances":{"quarkdrive-webdav":{"running":%s,"pid":%s}}}}\n' \
        "$1" "${QD_TEST_PID:-999999999}" > "$QD_TEST_DIR/service.json"
}
start() {
    printf 'start\n' >> "$QD_TEST_DIR/events"
    QD_INSTANCE_OPENED=0
    # rc.common still closes the service after a start_service failure.
    start_service
    if [ "$QD_INSTANCE_OPENED" = 1 ]; then
        set_running "${QD_TEST_START_RUNNING:-true}"
    else
        printf '{}\n' > "$QD_TEST_DIR/service.json"
    fi
    service_triggers
    service_started
}
boot() { start; }
stop() {
    printf 'stop\n' >> "$QD_TEST_DIR/events"
    [ "$1" = quarkdrive-webdav ] || printf 'unregister\n' >> "$QD_TEST_DIR/events"
    [ "$QD_TEST_STOP_FAIL" != 1 ] || return 1
    if [ "$QD_TEST_STUCK" != 1 ] && [ -z "$QD_TEST_CLEANUP_AFTER" ]; then
        printf '{}\n' > "$QD_TEST_DIR/service.json"
    fi
    [ -z "$QD_TEST_PID" ] || kill -TERM "$QD_TEST_PID"
    [ "$QD_TEST_REMOVED_RACE" != 1 ]
}
restart() { stop; start; }
service_running() { grep -q '"running":true' "$QD_TEST_DIR/service.json"; }
ubus() {
    [ "$QD_TEST_QUERY_FAIL" != 1 ] || return 1
    cat "$QD_TEST_DIR/service.json"
}
sleep() {
    case "$1" in *[!0-9]*) return 1 ;; esac
    printf '%s\n' "$1" >> "$QD_TEST_DIR/waits"
    QD_WAITS=$((${QD_WAITS:-0} + 1))
    if [ -n "$QD_TEST_CLEANUP_AFTER" ] && [ "$QD_WAITS" -ge "$QD_TEST_CLEANUP_AFTER" ]; then
        printf '{}\n' > "$QD_TEST_DIR/service.json"
    fi
    if [ -n "$QD_TEST_START_AFTER" ] && [ "$QD_WAITS" -ge "$QD_TEST_START_AFTER" ]; then
        set_running true
    fi
    [ "$QD_TEST_REAL_WAIT" != 1 ] || /bin/sleep "$1"
    return 0
}
. "$1"
eval "$QD_TEST_ACTION"
'''

JSONFILTER = '''
import json, sys
instances = json.load(sys.stdin).get('quarkdrive-webdav', {}).get('instances', {})
for instance in instances.values():
    if sys.argv[-1].endswith('.pid'):
        if 'pid' in instance:
            print(instance['pid'])
    else:
        print(json.dumps(instance))
'''

CHILD = '''
import signal, sys, time
def stop(*args):
    time.sleep(float(sys.argv[1]))
    sys.exit(0)
signal.signal(signal.SIGTERM, stop)
print('ready', flush=True)
while True:
    signal.pause()
'''


class InitTests(unittest.TestCase):
    def run_action(self, action, enabled='0', active=False, **overrides):
        with tempfile.TemporaryDirectory(prefix='quarkdrive-init-test-') as directory:
            root = Path(directory)
            binary = root / 'quarkdrive-webdav'
            binary.write_text('#!/bin/sh\nexit 0\n')
            binary.chmod(0o700)
            parser = root / 'jsonfilter'
            parser.write_text('#!' + sys.executable + '\n' + JSONFILTER)
            parser.chmod(0o700)
            state = {'quarkdrive-webdav': {'instances': {'quarkdrive-webdav': {
                'running': True, 'pid': int(overrides.get('PID', 999999999))
            }}}} if active else {}
            (root / 'service.json').write_text(json.dumps(state, separators=(',', ':')))
            config = dict(enabled=enabled, binary_path=str(binary), quark_cookie='test-cookie',
                          ACTION=action, DIR=directory)
            config.update(overrides)
            env = dict(os.environ, LC_ALL='C', PATH=directory + ':' + os.environ['PATH'])
            env.update(('QD_TEST_' + key, str(value)) for key, value in config.items())
            result = subprocess.run(['/bin/sh', '-c', HARNESS, 'test', str(INIT)], env=env,
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                    universal_newlines=True, timeout=15)
            events = (root / 'events').read_text().splitlines() if (root / 'events').exists() else []
            waits = (root / 'waits').read_text().splitlines() if (root / 'waits').exists() else []
            state = json.loads((root / 'service.json').read_text())
            return result, events, waits, state

    def test_normal_and_boot_start_follow_enabled(self):
        for action in ['start', 'boot']:
            for enabled in ['0', '1']:
                with self.subTest(action=action, enabled=enabled):
                    result, events, waits, state = self.run_action(action, enabled=enabled)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual('instance' in events, enabled == '1')
                    self.assertIn('trigger', events)

    def test_reload_follows_enabled_from_both_runtime_states(self):
        for enabled in ['0', '1']:
            for active in [False, True]:
                with self.subTest(enabled=enabled, active=active):
                    result, events, waits, state = self.run_action('reload_service', enabled, active)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(bool(state), enabled == '1')
                    self.assertEqual('stop' in events, active)
                    self.assertIn('trigger', events)
                    if active and enabled == '1':
                        self.assertLess(events.index('stop'), events.index('instance'))

    def test_manual_start_and_restart_work_without_enabling(self):
        for action in ['start_now', 'restart_now']:
            with self.subTest(action=action):
                result, events, waits, state = self.run_action(action, active=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn('instance', events)
                self.assertTrue(state)

    def test_temporary_start_override_does_not_leak_to_normal_start(self):
        result, events, waits, state = self.run_action('start_now && start')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(events.count('instance'), 1)
        self.assertEqual(state, {})

    def test_disabled_start_and_apply_do_not_require_cookie_or_binary(self):
        for action in ['start', 'reload_service']:
            result, events, waits, state = self.run_action(action, active=True, quark_cookie='', binary_path='/missing')
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(state, {})

    def test_invalid_replacement_is_rejected_before_stopping(self):
        invalid = [dict(binary_path='/missing'), dict(quark_cookie=''), dict(port='0'),
                   dict(auth_user='user', auth_password='')]
        for action in ['restart_now', 'reload_service']:
            for config in invalid:
                with self.subTest(action=action, config=config):
                    result, events, waits, state = self.run_action(action, '1', True, **config)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertNotIn('stop', events)
                    self.assertTrue(state)

    def test_failed_start_validation_is_propagated_through_rc_common(self):
        result, events, waits, state = self.run_action('start', '1', quark_cookie='')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('cookie is required', result.stderr)
        self.assertNotIn('instance', events)

    def test_wait_for_running_handles_delays_and_timeout(self):
        result, events, waits, state = self.run_action('start_now', START_RUNNING='false', START_AFTER=2)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(waits, ['1', '1'])
        result, events, waits, state = self.run_action('start_now', START_RUNNING='false')
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(waits, ['1'] * 8)

    def test_repeated_stop_and_concurrent_removal_succeed(self):
        result, events, waits, state = self.run_action('stop_now && stop_now', active=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(events, ['stop'])
        result, events, waits, state = self.run_action('stop_now', active=True, REMOVED_RACE=1)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_manual_stop_preserves_the_reload_trigger_for_later_apply(self):
        result, events, waits, state = self.run_action('stop_now && reload_service', '1', True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn('unregister', events)
        self.assertTrue(state)

    def test_stop_waits_for_procd_cleanup_and_checks_after_last_wait(self):
        for delay in [2, 6]:
            result, events, waits, state = self.run_action('stop_now', active=True, CLEANUP_AFTER=delay)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(len(waits), delay)
            self.assertEqual(state, {})

    def test_query_failure_is_not_treated_as_stopped(self):
        result, events, waits, state = self.run_action('stop_now', QUERY_FAIL=1)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('Unable to query', result.stderr)

    def test_failed_stop_prevents_restart_or_apply_start(self):
        for action in ['restart_now', 'reload_service']:
            for mode in ['STOP_FAIL', 'STUCK']:
                result, events, waits, state = self.run_action(action, '1', True, **{mode: 1})
                self.assertNotEqual(result.returncode, 0)
                self.assertNotIn('start', events)
                self.assertTrue(state)

    def test_stop_waits_for_real_process_after_procd_removes_it(self):
        for delay in [1.2, 5.2]:
            child = subprocess.Popen([sys.executable, '-c', CHILD, str(delay)],
                                     stdout=subprocess.PIPE, universal_newlines=True)
            reaper = threading.Thread(target=child.wait)
            try:
                self.assertEqual(child.stdout.readline().strip(), 'ready')
                reaper.start()
                result, events, waits, state = self.run_action('stop_now', active=True, PID=child.pid, REAL_WAIT=1)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIsNotNone(child.poll())
                self.assertTrue(waits)
                self.assertTrue(all(value == '1' for value in waits))
            finally:
                if child.poll() is None:
                    child.kill()
                reaper.join(timeout=3)
                child.stdout.close()


if __name__ == '__main__':
    unittest.main()
