"""Read LR/Rust startup logs without opening photos or changing the catalog."""
import argparse
from datetime import datetime
import json
from pathlib import Path
import re


def analyze(plugin_log, rust_logs):
    sessions = {}
    date = re.search(r'(\d{8})', plugin_log.name)
    for line in plugin_log.read_text(encoding='utf-8', errors='replace').splitlines():
        trace = re.search(r'\[startup\] id=(\S+) phase=(\S+) elapsed_ms=([\d.]+) unix_ms=(\d+)', line)
        if trace:
            sid, phase, elapsed, epoch = trace.groups()
            sessions.setdefault(sid, {}).setdefault('lr_phases', {})[phase] = {
                'elapsed_ms': float(elapsed), 'unix_ms': int(epoch),
            }
        elif '[run] ' in line and '--open ' in line:
            sid = re.search(r'--session-id ([\w-]+)', line)
            clock = re.match(r'(\d{2}:\d{2}:\d{2})', line)
            if sid and clock and date:
                value = datetime.strptime(date[1] + ' ' + clock[1], '%Y%m%d %H:%M:%S')
                sessions.setdefault(sid[1], {})['legacy_invoke_unix_ms'] = round(value.timestamp() * 1000)
    for path in rust_logs.glob('*.log'):
        lines = path.read_text(encoding='utf-8', errors='replace').splitlines()
        if not lines:
            continue
        header = lines[0]
        if '"--open-state-owned"' in header:
            role = 'gui'
        elif '"--open"' in header:
            role = 'launcher'
        else:
            continue
        sid = re.search(r'--session-id", "([\w-]+)', header) or re.search(r'session_([\w-]+)\.txt', header)
        if not sid:
            continue
        startup = {'first_log_unix_ms': int(header.split()[0])}
        for line in lines:
            epoch = int(line.split()[0])
            entry = re.search(r'startup entry unix_ms=(\d+) logger_ms=([\d.]+)', line)
            if entry:
                startup['entry_unix_ms'] = int(entry[1])
                startup['logger_ms'] = float(entry[2])
            if 'startup native-focus ' in line and 'foreground_verified=true' in line:
                startup.setdefault('foreground_verified_unix_ms',epoch)
            for text, key in [('first UI frame ', 'first_frame_unix_ms'), ('first loupe ', 'first_image_unix_ms'),
                              ('startup native-window begin', 'native_begin_unix_ms'),
                              ('startup app-builder begin', 'app_builder_unix_ms')]:
                if text in line:
                    startup.setdefault(key, epoch)
            for text, key in [('font-config', 'font_config_ms'), ('worker-pool', 'worker_pool_ms'),
                              ('session-attached', 'session_ms')]:
                match = re.search(r'startup ' + text + r' elapsed_ms=([\d.]+)', line)
                if match:
                    startup[key] = float(match[1])
        sessions.setdefault(sid[1], {})[role] = startup
    for result in sessions.values():
        launcher = result.get('launcher', {})
        gui = result.get('gui', {})
        entry = launcher.get('entry_unix_ms', launcher.get('first_log_unix_ms'))
        if entry is not None and 'legacy_invoke_unix_ms' in result:
            gap = entry - result['legacy_invoke_unix_ms']
            result['legacy_invoke_to_launcher_log_ms_range'] = [gap - 999, gap]
        phases = result.get('lr_phases', {})
        for phase_a, phase_b, key in [('menu', 'task', 'task_queue_ms'), ('task', 'selection', 'selection_ms'),
                                      ('selection', 'metadata', 'metadata_ms'), ('invoke', 'popen-return', 'popen_ms'),
                                      ('popen-return', 'read-return', 'pipe_read_ms'),
                                      ('execute-start', 'execute-return', 'sdk_execute_ms')]:
            if phase_a in phases and phase_b in phases:
                result[key] = round(phases[phase_b]['elapsed_ms'] - phases[phase_a]['elapsed_ms'], 1)
        if 'invoke' in phases and entry is not None:
            result['invoke_to_program_entry_ms'] = entry - phases['invoke']['unix_ms']
        if 'menu' in phases and 'first_frame_unix_ms' in gui:
            result['menu_to_first_frame_ms'] = gui['first_frame_unix_ms'] - phases['menu']['unix_ms']
        if 'menu' in phases and 'first_image_unix_ms' in gui:
            result['menu_to_first_image_ms'] = gui['first_image_unix_ms'] - phases['menu']['unix_ms']
        gui_entry = gui.get('entry_unix_ms', gui.get('first_log_unix_ms'))
        if gui_entry is not None and 'first_frame_unix_ms' in gui:
            result['gui_entry_to_first_frame_ms'] = gui['first_frame_unix_ms'] - gui_entry
        if 'native_begin_unix_ms' in gui and 'app_builder_unix_ms' in gui:
            result['graphics_window_init_ms'] = gui['app_builder_unix_ms'] - gui['native_begin_unix_ms']
    return sessions


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--plugin-log', type=Path, required=True)
    parser.add_argument('--rust-logs', type=Path, required=True)
    parser.add_argument('--session')
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    results = analyze(args.plugin_log, args.rust_logs)
    if args.session:
        results = {args.session: results[args.session]}
    text = json.dumps(results, ensure_ascii=False, indent=2) + '\n'
    print(text, end='')
    if args.output:
        args.output.write_text(text, encoding='utf-8')
