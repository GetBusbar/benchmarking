import importlib.util
import json
import subprocess
import sys
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
 def test_two_batches_publish_together_once(self):
  with tempfile.TemporaryDirectory() as temporary:
   root=Path(temporary);publication=root/'publication';(publication/'results').mkdir(parents=True)
   (publication/'results/active-field.json').write_text(json.dumps({'engine_commit':'new','started_at':'2026-11-08T00:00:00Z','gateways':['busbar','bifrost']}))
   batches=[]
   for gateway in ['busbar','bifrost']:
    source=root/gateway;(source/'results/snapshots').mkdir(parents=True)
    path=source/'results/snapshots'/f'{gateway}.json'
    path.write_text(json.dumps({'gateway':gateway,'measured_at':'2026-11-08T01:00:00Z','rig':{'engine':{'commit':'new'},'box_qualify':{'outcome':'pass'}},'matrix':{'cells':{}}}))
    listing=source/'completed.txt';listing.write_text(str(path)+'\n');batches.append((source,listing))
   command=[sys.executable,str(Path(__file__).with_name('publish-results.py')),'--source',str(batches[0][0]),'--measured-files',str(batches[0][1]),'--additional-source',str(batches[1][0]),str(batches[1][1]),'--publication',str(publication),'--state',str(root/'state.json'),'--',sys.executable,'-c',"from pathlib import Path; assert len(list(Path('results/snapshots').glob('*.json')))==2; p=Path('calls'); p.write_text(p.read_text()+'x' if p.exists() else 'x')"]
   subprocess.run(command,check=True,capture_output=True)
   subprocess.run(command,check=True,capture_output=True)
   self.assertEqual((publication/'calls').read_text(),'x')
   self.assertEqual(len(json.loads((root/'state.json').read_text())),2)
if __name__=='__main__':unittest.main()
