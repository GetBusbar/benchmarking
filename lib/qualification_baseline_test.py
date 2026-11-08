import unittest
from qualification_baseline import median_for

class BaselineTests(unittest.TestCase):
    def record(self,engine='new',mock='mock',rate=100,outcome='pass'):
        return {'rig':{'engine':{'commit':engine},'mock':{'sha256':mock},'box_qualify':{'observed_rps':rate,'outcome':outcome}}}
    def test_old_instrument_and_failed_boxes_cannot_seed_new_baseline(self):
        rows=[self.record(engine='old',rate=500),self.record(mock='old',rate=500),self.record(rate=1,outcome='fail'),self.record(rate=90),self.record(rate=110)]
        self.assertEqual(median_for(rows,'new','mock'),100)
        self.assertIsNone(median_for(rows,'unknown','mock'))
    def test_absence_is_not_zero_or_a_valid_reference(self):
        self.assertIsNone(median_for([self.record(rate=None),self.record(rate=True),self.record(rate=0)],'new','mock'))
if __name__=='__main__':unittest.main()
