import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('benchmark_load', Path(__file__).resolve().parents[1] / 'scripts/benchmark-load.py')
benchmark = importlib.util.module_from_spec(spec)
spec.loader.exec_module(benchmark)


class LoadSamplerTests(unittest.TestCase):
    def test_descendants_exclude_unrelated_processes_and_report_missing_root(self):
        rows = [(10, 1, 100), (11, 10, 50), (12, 11, 25), (99, 1, 9000)]
        self.assertEqual(benchmark.descendant_memory(rows, 10), {
            'root_rss_bytes': 102400, 'tree_rss_sum_bytes': 179200, 'process_count': 3})
        self.assertIsNone(benchmark.descendant_memory(rows, 42))
        self.assertIsNone(benchmark.descendant_memory([(10, 1, 0)], 10))

    def test_phase_assignment_and_unsampled_phase_is_null_not_zero(self):
        samples = [{'at_us': 12, 'root_rss_bytes': 100, 'tree_rss_sum_bytes': 200},
                   {'at_us': 20, 'root_rss_bytes': 150, 'tree_rss_sum_bytes': 150}]
        events = [{'at_us': 0, 'phase': 'read'}, {'at_us': 10, 'phase': 'parse'},
                  {'at_us': 20, 'phase': 'done'}, {'at_us': 20, 'phase': 'first_snap'}]
        memory = benchmark.annotate_memory({'phases': [{'phase': p} for p in ['read', 'parse', 'vertices']]}, samples, events)
        self.assertEqual(samples[0]['phase'], 'parse')
        self.assertEqual(samples[1]['phase'], 'first_snap')
        self.assertEqual(memory['parse']['sampled_peak_tree_rss_sum_bytes'], 200)
        self.assertEqual(memory['read']['samples'], 0)
        self.assertIsNone(memory['read']['sampled_peak_root_rss_bytes'])
        self.assertIsNone(memory['vertices']['sampled_peak_tree_rss_sum_bytes'])
        with self.assertRaises(ValueError):
            benchmark.annotate_memory({'phases': []}, [], list(reversed(events)))

    def test_missing_phase_is_not_treated_as_zero_time(self):
        records = [{'profile': {'total_us':100, 'phases':[{'phase':'parse','elapsed_us':80}]},
                    'memory':{'sampled_peak_tree_rss_sum_bytes':None}},
                   {'profile': {'total_us':200,'phases':[]},
                    'memory':{'sampled_peak_tree_rss_sum_bytes':300}}]
        summary = benchmark.summarize(records)
        self.assertEqual(summary['median_total_us'], 150)
        self.assertEqual(summary['median_phase_us']['parse'], 80)
        self.assertEqual(summary['phase_observed_runs']['parse'], 1)
        self.assertEqual(summary['max_sampled_tree_rss_sum_bytes'], 300)


if __name__ == '__main__':
    unittest.main()
