import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
spec=importlib.util.spec_from_file_location('publisher',Path(__file__).with_name('publish-results.py'));m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
class PublicationTests(unittest.TestCase):
 def test_completion_list_does_not_admit_old_or_failed_observations(self):
  with tempfile.TemporaryDirectory() as temporary:
   root=Path(temporary);(root/'results/snapshots').mkdir(parents=True)
   (root/'results/active-field.json').write_text(json.dumps({'engine_commit':'new','started_at':'2026-11-08T00:00:00Z','gateways':['busbar']}))
   path=root/'results/snapshots/result_busbar.json';listing=root/'completed.txt';listing.write_text(str(path)+'\n')
   record={'gateway':'busbar','measured_at':'2026-11-08T01:00:00Z','rig':{'engine':{'commit':'new'},'box_qualify':{'outcome':'pass'}},'matrix':{'cells':{}}}
   path.write_text(json.dumps(record));self.assertEqual(len(m.collect(root,root,[listing])),1)
   record['rig']['engine']['commit']='old';path.write_text(json.dumps(record));self.assertEqual(m.collect(root,root,[listing]),[])
   record['rig']['engine']['commit']='new';record['rig']['box_qualify']['outcome']='fail';path.write_text(json.dumps(record))
   with self.assertRaises(ValueError):m.collect(root,root,[listing])
if __name__=='__main__':unittest.main()
