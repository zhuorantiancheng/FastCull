import tempfile
import unittest
from datetime import datetime
from pathlib import Path
from scripts.analyze_startup import analyze


class StartupAnalysisTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='fastcull-trace-')
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.log = self.root / 'fastcull_plugin_20261012.log'
        self.epoch = round(datetime(2026, 10, 12, 22, 0).timestamp() * 1000)

    def test_legacy_seconds_have_an_uncertainty_range_not_false_precision(self):
        self.log.write_text('22:00:00 [run] "fastcull.exe" --open "state" --session-id old-id\n', encoding='utf-8')
        (self.root/'parent.log').write_text(f'{self.epoch+5500} INFO args=["fastcull.exe", "--open", "state", "--session-id", "old-id"]\n', encoding='utf-8')
        report=analyze(self.log,self.root)['old-id']
        self.assertEqual(report['legacy_invoke_to_launcher_log_ms_range'],[4501,5500])
        self.assertNotIn('menu_to_first_frame_ms',report)

    def test_joins_session_processes_and_includes_wait_before_logger(self):
        self.log.write_text(
            f'[startup] id=new-id phase=menu elapsed_ms=0.0 unix_ms={self.epoch}\n'
            f'[startup] id=new-id phase=task elapsed_ms=100.0 unix_ms={self.epoch+100}\n'
            f'[startup] id=new-id phase=invoke elapsed_ms=200.0 unix_ms={self.epoch+200}\n'
            f'[startup] id=new-id phase=popen-return elapsed_ms=5200.0 unix_ms={self.epoch+5200}\n'
            f'[startup] id=new-id phase=read-return elapsed_ms=5300.0 unix_ms={self.epoch+5300}\n', encoding='utf-8')
        (self.root/'parent.log').write_text(
            f'{self.epoch+5210} INFO args=["fastcull.exe", "--open", "state", "--session-id", "new-id"]\n'
            f'{self.epoch+5210} INFO startup entry unix_ms={self.epoch+5100} logger_ms=110.0\n', encoding='utf-8')
        (self.root/'child.log').write_text(
            f'{self.epoch+5250} INFO args=["fastcull.exe", "--open-state-owned", "state", "--session", "session_new-id.txt"]\n'
            f'{self.epoch+5250} INFO startup entry unix_ms={self.epoch+5240} logger_ms=10.0\n'
            f'{self.epoch+5260} INFO startup native-window begin\n'
            f'{self.epoch+5700} INFO startup app-builder begin\n'
            f'{self.epoch+5800} INFO first UI frame 1.0ms\n', encoding='utf-8')
        # Another session must not be mistaken for this request's GUI.
        (self.root/'other.log').write_text(f'{self.epoch+100} INFO args=["fastcull.exe", "--open", "state", "--session-id", "other-id"]\n', encoding='utf-8')
        report=analyze(self.log,self.root)['new-id']
        self.assertEqual(report['task_queue_ms'],100)
        self.assertEqual(report['popen_ms'],5000)
        self.assertEqual(report['pipe_read_ms'],100)
        self.assertEqual(report['invoke_to_program_entry_ms'],4900)
        self.assertEqual(report['menu_to_first_frame_ms'],5800)
        self.assertEqual(report['gui_entry_to_first_frame_ms'],560)
        self.assertEqual(report['graphics_window_init_ms'],440)


if __name__ == '__main__':
    unittest.main()
