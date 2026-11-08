import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch
from bench_network import cleanup
from bench_placement import select, capacity
from aws_sdk import expected_account, verify_account

class CloudTests(unittest.TestCase):
    def test_cheapest_can_split_regions_without_changing_hardware(self):
        prices=[{'region':'cheap','estimated_hour_usd':1,'instance_type':'m7g.4xlarge'},{'region':'next','estimated_hour_usd':2,'instance_type':'m7g.4xlarge'}]
        capacity={'cheap':{'slots':1,'zone':'cheap-a'},'next':{'slots':3,'zone':'next-a'}}
        rows=select(['bifrost','busbar'],prices,capacity,'cheapest')
        self.assertEqual([r['region'] for r in rows],['cheap','next'])
        self.assertEqual({r['instance_type'] for r in rows},{'m7g.4xlarge'})
        self.assertEqual({r['region'] for r in select(['bifrost','busbar'],prices,capacity,'same-region')},{'next'})
    def test_full_vpc_region_is_skipped_before_placement(self):
        sdk=Mock();ec2=Mock();quotas=Mock()
        sdk.client.side_effect=lambda service,**kwargs:ec2 if service=='ec2' else quotas
        quotas.get_service_quota.return_value={'Quota':{'Value':5}}
        ec2.describe_vpcs.return_value={'Vpcs':[{}]*5}
        self.assertEqual(capacity(sdk,'cheap','m7g.4xlarge')['slots'],0)
        ec2.describe_instance_type_offerings.assert_not_called()
        rows=select(['rust'],[{'region':'cheap'},{'region':'next'}],{'cheap':{'slots':0},'next':{'slots':1,'zone':'next-a'}},'cheapest')
        self.assertEqual(rows[0]['region'],'next')
    def test_insufficient_quota_never_silently_changes_instance_type(self):
        with self.assertRaises(ValueError):select(['a'],[{'region':'cheap'}],{'cheap':{'slots':0}},'cheapest')
    def test_cleanup_refuses_someone_elses_vpc(self):
        client=Mock();client.describe_vpcs.return_value={'Vpcs':[{'Tags':[{'Key':'purpose','Value':'modernci'}]}]}
        with self.assertRaises(ValueError):cleanup(client,{'vpc_id':'vpc-other','run_id':'mine'},Path('/unused'))
        client.delete_vpc.assert_not_called()
    def test_cleanup_waits_for_instance_harvest(self):
        client=Mock();client.describe_vpcs.return_value={'Vpcs':[{'Tags':[{'Key':'purpose','Value':'gateway-bench'},{'Key':'run','Value':'mine'}]}]}
        client.describe_instances.return_value={'Reservations':[{'Instances':[{'InstanceId':'i-live'}]}]}
        with self.assertRaises(RuntimeError):cleanup(client,{'vpc_id':'vpc-owned','run_id':'mine'},Path('/unused'))
        client.delete_vpc.assert_not_called()
    def test_account_selection_is_explicit_and_verified(self):
        with patch.dict(os.environ,{},clear=True):
            with self.assertRaises(ValueError):expected_account()
        client=Mock();client.client.return_value.get_caller_identity.return_value={'Account':'999999999999'}
        with patch.dict(os.environ,{'OTB_AWS_ACCOUNT_ID':'123456789012'}):
            with self.assertRaises(ValueError):verify_account(client)

if __name__=='__main__':unittest.main()
