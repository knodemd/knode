import copy
import unittest
import evaluate

class EvaluationTests(unittest.TestCase):

    def data(self):
        run = {'task': 'resume', 'trial': '1', 'scope': 'end-to-end', 'includes_memory_writes': True, 'success': True, 'evidence': 'synthetic test fixture, not a model run', 'usage': None}
        return {'schema_version': 1, 'model': 'test-model-exact-version', 'corpus_sha256': 'test-hash', 'runs': [dict(run, arm='files-selective'), dict(run, arm='knode')]}

    def check(self, data):
        return evaluate.report(data, 'test-hash', {'resume'})

    def test_missing_usage_is_unknown_not_zero_or_estimated(self):
        result = self.check(self.data())
        pair = result['paired_comparisons'][0]
        self.assertEqual(pair['paired_runs'], 1)
        self.assertIsNone(pair['tokens_saved']['mean'])
        self.assertEqual(pair['tokens_saved']['measured_runs'], 0)

    def test_cache_cost_does_not_double_count_and_includes_output(self):
        data = self.data()
        data['prices'] = {'currency': 'USD', 'as_of': 'fixture-date', 'source': 'fixture-only', 'input_per_million': 2, 'cached_input_per_million': 1, 'output_per_million': 4}
        data['runs'][0]['usage'] = {'input_tokens': 100, 'cached_input_tokens': 60, 'output_tokens': 20, 'reasoning_output_tokens': 10}
        data['runs'][1]['usage'] = {'input_tokens': 40, 'cached_input_tokens': 0, 'output_tokens': 10}
        result = self.check(data)
        self.assertEqual(result['paired_comparisons'][0]['tokens_saved']['mean'], 70)
        self.assertAlmostEqual(result['arms']['files-selective']['estimated_usd']['mean'], 0.00022)

    def test_reject_duplicate_runs_wrong_corpus_invalid_usage_and_missing_write_overhead(self):
        for mutate in [lambda d: d['runs'].append(copy.deepcopy(d['runs'][0])), lambda d: d.update(corpus_sha256='wrong'), lambda d: d['runs'][0].update(includes_memory_writes=False), lambda d: d['runs'][0].update(usage={'input_tokens': 10, 'cached_input_tokens': 11, 'output_tokens': 1})]:
            data = self.data()
            mutate(data)
            with self.assertRaises(ValueError):
                self.check(data)

    def test_failed_candidate_is_reported_even_with_lower_tokens(self):
        data = self.data()
        data['runs'][1]['success'] = False
        result = self.check(data)['paired_comparisons'][0]
        self.assertEqual(result['baseline_successes'], 1)
        self.assertEqual(result['candidate_successes'], 0)

    def test_unpaired_tasks_do_not_produce_savings(self):
        data = self.data()
        data['runs'][1]['trial'] = '2'
        self.assertEqual(self.check(data)['paired_comparisons'], [])
if __name__ == '__main__':
    unittest.main()
