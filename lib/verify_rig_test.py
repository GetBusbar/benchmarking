import hashlib
import os
import tempfile
import unittest
from pathlib import Path
from verify_rig import verify
from aws_sdk import request, parse_options

class RigTests(unittest.TestCase):
    def test_bundle_rejects_tampering_and_wrong_commit(self):
        with tempfile.TemporaryDirectory(dir=os.environ.get('TMPDIR')) as tmp:
            root=Path(tmp); (root/'engine-commit').write_text('a'*40)
            binary=b'\x7fELF\x02\x01'+bytes(12)+(183).to_bytes(2,'little')
            for name in ['otb-arm64','mock-arm64']: (root/name).write_bytes(binary)
            (root/'SHA256SUMS').write_text(''.join(f'{hashlib.sha256(binary).hexdigest()}  rig/{name}\n' for name in ['otb-arm64','mock-arm64']))
            verify(root,'a'*40,'arm64')
            with self.assertRaises(ValueError): verify(root,'b'*40,'arm64')
            (root/'mock-arm64').write_bytes(binary+b'bad')
            with self.assertRaises(ValueError): verify(root,'a'*40,'arm64')
    def test_sdk_launch_has_imdsv2_and_one_instance(self):
        r=request('run-instances',parse_options(['--image-id','ami-test','--instance-type','m7g.4xlarge']))
        self.assertEqual(r['MaxCount'],1)
        self.assertEqual(r['MetadataOptions']['HttpTokens'],'required')
    def test_security_group_tagged_on_create(self):
        r=request('create-security-group',parse_options(['--group-name','gateway-bench-test']))
        self.assertIn({'Key':'purpose','Value':'gateway-bench'},r['TagSpecifications'][0]['Tags'])
        self.assertIn({'Key':'managed-by','Value':'gateway-bench'},r['TagSpecifications'][0]['Tags'])
    def test_unknown_options_fail_closed(self):
        with self.assertRaises(ValueError): request('run-instances',parse_options(['--profile','other']))

if __name__=='__main__': unittest.main()
