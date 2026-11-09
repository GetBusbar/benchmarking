#!/usr/bin/env python3
"""Run a pinned benchmark field with live-priced placement and owned networks."""
import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
import datetime
import json
import os
from pathlib import Path
import signal
import subprocess
import sys

ROOT=Path(__file__).resolve().parent
sys.path.insert(0,str(ROOT/'lib'))
from aws_sdk import session, verify_account
from bench_placement import live_prices, capacity, select
from bench_network import create, cleanup, save


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('gateways',nargs='*')
    parser.add_argument('--placement',choices=['same-region','cheapest'],default='same-region')
    parser.add_argument('--region',help='Optional explicit region; otherwise choose from current prices')
    parser.add_argument('--state-dir',type=Path,required=True,help='Private directory outside the source tree for logs and SSH keys')
    parser.add_argument('--rig-dir',type=Path,required=True)
    parser.add_argument('--plan-only',action='store_true')
    args=parser.parse_args()
    directory=args.state_dir.resolve()
    if directory==ROOT or ROOT in directory.parents:parser.error('state-dir must be outside the source tree')
    directory.mkdir(parents=True,exist_ok=True)
    sdk=session(args.region or os.environ.get('OTB_AWS_REGION') or 'us-east-1');verify_account(sdk)
    gateways=args.gateways or sorted(p.parent.name for p in (ROOT/'gateways').glob('*/definition.json'))
    if not gateways or any(not (ROOT/'gateways'/g/'definition.json').is_file() for g in gateways):parser.error('Unknown gateway')
    prices=live_prices(sdk)
    if args.region:prices=[p for p in prices if p['region']==args.region]
    capacities={}
    # Query only as many cheapest eligible regions as needed to fit this field.
    for row in prices:
        try:capacities[row['region']]=capacity(sdk,row['region'],row['instance_type'])
        except Exception as exc:
            print(f"Region {row['region']} unavailable for placement: {exc}",file=sys.stderr,flush=True)
            continue
        slots=[v['slots'] for v in capacities.values()]
        if (max(slots,default=0) if args.placement=='same-region' else sum(slots))>=len(gateways):break
    assignments=select(gateways,prices,capacities,args.placement)
    stamp=datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ')
    run_id='otb-'+stamp
    plan={'run_id':run_id,'source_commit':subprocess.check_output(['git','-C',str(ROOT),'rev-parse','HEAD'],text=True).strip(),'engine_commit':(args.rig_dir/'engine-commit').read_text().strip(),'placement':args.placement,'assignments':assignments}
    save(directory/'plan.json',plan)
    for a in assignments:print(f"{a['gateway']}: {a['region']} {a['zone']} {a['instance_type']} estimated ${a['estimated_hour_usd']:.4f}/hour",flush=True)
    if args.plan_only:return 0
    networks={};children=[];images={}
    def interrupt(signum,frame):
        # Send exactly one signal in finally: a second TERM can interrupt the
        # shell's teardown trap while its instances are still billing. Later
        # signals are ignored so a repeated Ctrl-C cannot skip network cleanup.
        signal.signal(signal.SIGTERM,signal.SIG_IGN);signal.signal(signal.SIGINT,signal.SIG_IGN)
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM,interrupt);signal.signal(signal.SIGINT,interrupt)
    try:
        for region in sorted({a['region'] for a in assignments}):
            group=[a for a in assignments if a['region']==region]
            client=sdk.client('ec2',region_name=region)
            parameter='/aws/service/canonical/ubuntu/server/24.04/stable/current/arm64/hvm/ebs-gp3/ami-id'
            ami=sdk.client('ssm',region_name=region).get_parameter(Name=parameter)['Parameter']['Value']
            image=client.describe_images(ImageIds=[ami])['Images'][0]
            if images and image['Name'] not in {v['name'] for v in images.values()}:
                raise ValueError('Regional Ubuntu image builds differ; refusing to mix operating systems in the field')
            images[region]={'ami_id':ami,'name':image['Name'],'owner_id':image['OwnerId']}
            plan['images']=images;save(directory/'plan.json',plan)
            journal=directory/f'network-{region}.json' 
            state=create(client,run_id,group[0]['zone'],journal);networks[region]=(client,state,journal)
            env={**os.environ,'OTB_AWS_REGION':region,'AWS_DEFAULT_REGION':region,'BENCH_AMI_ID':ami,'BENCH_VPC_ID':state['vpc_id'],'BENCH_SUBNET_ID':state['subnet_id'],'BENCH_STATE_DIR':str(directory/region),'BENCH_RIG_DIR':str(args.rig_dir.resolve()),'RUN_ID':run_id+'-'+region,'PUBLISH':'0'}
            log=(directory/f'run-{region}.log').open('w')
            # Own session: a terminal Ctrl-C reaches only this orchestrator, which then
            # signals the child exactly once below.
            child=subprocess.Popen(['bash',str(ROOT/'run-on-ec2.sh'),*[a['gateway'] for a in group]],env=env,cwd=ROOT,stdout=log,stderr=subprocess.STDOUT,start_new_session=True)
            log.close();children.append(child)
        failed=0
        for child in children:failed+=child.wait()!=0
        return 1 if failed else 0
    finally:
        for child in children:
            if child.poll() is None:
                child.send_signal(signal.SIGTERM)
                child.wait()
        errors=[]
        for region,(client,state,journal) in networks.items():
            try:cleanup(client,state,journal)
            except Exception as exc:errors.append(f'{region}: {exc}; retain journal {journal}')
        if errors:raise RuntimeError('Network cleanup incomplete: '+'; '.join(errors))

if __name__=='__main__':
    sys.exit(main())
