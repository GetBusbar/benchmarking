#!/usr/bin/env python3
"""Run a pinned benchmark field with live-priced placement and owned networks.

Placement modes:
  same-region / cheapest   one fresh box per gateway (the published method).
  per-cell                 one fresh box per measured cell, in batches that fit the regional vCPU
                           quota (read from Service Quotas at runtime, less a safety margin). Each
                           gateway's cells are merged into one snapshot (lib/merge_cells.py).
                           See docs/per-cell-boxes.md.

Candidate check (per-cell only): --candidate-image runs the named --cells of ONE gateway with an
overridden image on the pinned instrument and writes to results/candidates/, never results/snapshots/,
with a comparison report against the published result (lib/candidate_report.py).

--dry-run prints the per-cell batch schedule, wall time and cost and launches nothing. With
--quota-vcpus and --hour-usd it makes no AWS call at all.
"""
import argparse
import datetime
import json
import os
from pathlib import Path
import signal
import subprocess
import sys

ROOT=Path(__file__).resolve().parent
sys.path.insert(0,str(ROOT/'lib'))
from bench_network import create, cleanup, save
import per_cell
import merge_cells

INSTANCE_TYPE='m7g.4xlarge'


def aws():
    from aws_sdk import session, verify_account
    from bench_placement import live_prices, capacity, select
    return session, verify_account, live_prices, capacity, select


def box_budget(quota_vcpus, in_use_vcpus, margin_vcpus, cap=None):
    """Concurrent boxes that fit: (quota - running on-demand vCPUs - safety margin) / 16, capped."""
    boxes=int(max(0,quota_vcpus-in_use_vcpus-margin_vcpus)//per_cell.VCPUS_PER_BOX)
    return min(boxes,cap) if cap else boxes


def field_units(gateways, cells=None):
    units=[]
    for g in gateways:
        units+=per_cell.units_for(g,per_cell.load_manifest(ROOT,g),cells)
    timings={g:per_cell.previous_timings(ROOT,g) for g in gateways}
    return per_cell.estimate(units,timings)


def launch_region(sdk, region):
    client=sdk.client('ec2',region_name=region)
    parameter='/aws/service/canonical/ubuntu/server/24.04/stable/current/arm64/hvm/ebs-gp3/ami-id'
    ami=sdk.client('ssm',region_name=region).get_parameter(Name=parameter)['Parameter']['Value']
    image=client.describe_images(ImageIds=[ami])['Images'][0]
    return client,{'ami_id':ami,'name':image['Name'],'owner_id':image['OwnerId']}


def main():
    parser=argparse.ArgumentParser(description=__doc__,formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('gateways',nargs='*')
    parser.add_argument('--placement',choices=['same-region','cheapest','per-cell'],default='same-region')
    parser.add_argument('--region',help='Optional explicit region; otherwise choose from current prices')
    parser.add_argument('--state-dir',type=Path,help='Private directory outside the source tree for logs and SSH keys (required unless --dry-run)')
    parser.add_argument('--rig-dir',type=Path,help='Prebuilt rig (required unless --dry-run)')
    parser.add_argument('--plan-only',action='store_true')
    parser.add_argument('--qualify-band-pct',type=float,help='Also refuse a box whose qualification drift is below -X%% (one-sided). Default: the engine band only (20%%)')
    per=parser.add_argument_group('per-cell placement')
    per.add_argument('--cells',help='Comma-separated ingress>egress cells to run (default: every measured cell)')
    per.add_argument('--candidate-image',help='Candidate check: gateway image ref, or docker-archive:<path> from `docker save`')
    per.add_argument('--quota-margin-vcpus',type=float,default=32,help='vCPUs of the quota left unused (default 32 = two boxes)')
    per.add_argument('--max-boxes',type=int,help='Cap concurrent boxes below what the quota allows')
    per.add_argument('--dry-run',action='store_true',help='Print the batch schedule, wall time and cost; launch nothing')
    per.add_argument('--quota-vcpus',type=float,help='Dry-run only: assume this vCPU quota (nothing in use) instead of reading Service Quotas')
    per.add_argument('--hour-usd',type=float,help='Dry-run only: assume this per-box hourly price instead of the Price List API')
    args=parser.parse_args()
    if args.placement!='per-cell' and (args.cells or args.candidate_image or args.dry_run):
        parser.error('--cells, --candidate-image and --dry-run need --placement per-cell')
    if (args.quota_vcpus or args.hour_usd) and not args.dry_run:
        parser.error('--quota-vcpus/--hour-usd are dry-run assumptions; a live run reads quota and prices from AWS')
    if args.qualify_band_pct is not None and not 0<args.qualify_band_pct<=20:
        parser.error('--qualify-band-pct must be in (0, 20]: it can only tighten the engine band')
    gateways=args.gateways or sorted(p.parent.name for p in (ROOT/'gateways').glob('*/definition.json'))
    if not gateways or any(not (ROOT/'gateways'/g/'definition.json').is_file() for g in gateways):parser.error('Unknown gateway')
    cells=[c.strip() for c in args.cells.split(',') if c.strip()] if args.cells else None
    if args.candidate_image and (len(gateways)!=1 or not cells):
        parser.error('a candidate check names exactly one gateway and its --cells')
    if args.candidate_image and not args.candidate_image.startswith('docker-archive:'):
        ref=args.candidate_image
        if '@sha256:' not in ref:print(f'note: {ref} is not digest-pinned; the box records the digest it actually pulled',file=sys.stderr)
    if not args.dry_run and (args.state_dir is None or args.rig_dir is None):
        parser.error('--state-dir and --rig-dir are required unless --dry-run')
    if args.placement=='per-cell':
        return per_cell_field(args,gateways,cells)
    return per_gateway_field(args,gateways)


def per_gateway_field(args, gateways):
    session,verify_account,live_prices,capacity,select=aws()
    directory=args.state_dir.resolve()
    if directory==ROOT or ROOT in directory.parents:raise SystemExit('state-dir must be outside the source tree')
    directory.mkdir(parents=True,exist_ok=True)
    sdk=session(args.region or os.environ.get('OTB_AWS_REGION') or 'us-east-1');verify_account(sdk)
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
    install_interrupt()
    try:
        for region in sorted({a['region'] for a in assignments}):
            group=[a for a in assignments if a['region']==region]
            client,image=launch_region(sdk,region)
            if images and image['name'] not in {v['name'] for v in images.values()}:
                raise ValueError('Regional Ubuntu image builds differ; refusing to mix operating systems in the field')
            images[region]=image
            plan['images']=images;save(directory/'plan.json',plan)
            journal=directory/f'network-{region}.json'
            state=create(client,run_id,group[0]['zone'],journal);networks[region]=(client,state,journal)
            env=child_env(args,directory/region,region,image['ami_id'],state,run_id+'-'+region)
            log=(directory/f'run-{region}.log').open('w')
            # Own session: a terminal Ctrl-C reaches only this orchestrator, which then
            # signals the child exactly once below.
            child=subprocess.Popen(['bash',str(ROOT/'run-on-ec2.sh'),*[a['gateway'] for a in group]],env=env,cwd=ROOT,stdout=log,stderr=subprocess.STDOUT,start_new_session=True)
            log.close();children.append(child)
        failed=0
        for child in children:failed+=child.wait()!=0
        return 1 if failed else 0
    finally:
        stop_children(children)
        cleanup_networks(networks)


def install_interrupt():
    def interrupt(signum,frame):
        # Send exactly one signal in finally: a second TERM can interrupt the
        # shell's teardown trap while its instances are still billing. Later
        # signals are ignored so a repeated Ctrl-C cannot skip network cleanup.
        signal.signal(signal.SIGTERM,signal.SIG_IGN);signal.signal(signal.SIGINT,signal.SIG_IGN)
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM,interrupt);signal.signal(signal.SIGINT,interrupt)


def child_env(args, state_dir, region, ami, network, run_id, extra=None):
    env={**os.environ,'OTB_AWS_REGION':region,'AWS_DEFAULT_REGION':region,'BENCH_AMI_ID':ami,'BENCH_VPC_ID':network['vpc_id'],'BENCH_SUBNET_ID':network['subnet_id'],'BENCH_STATE_DIR':str(state_dir),'BENCH_RIG_DIR':str(args.rig_dir.resolve()),'RUN_ID':run_id,'PUBLISH':'0'}
    if args.qualify_band_pct is not None:env['BENCH_QUALIFY_BAND_PCT']=f'{args.qualify_band_pct:g}'
    env.update(extra or {})
    return env


def stop_children(children):
    for child in children:
        if child.poll() is None:
            child.send_signal(signal.SIGTERM)
            child.wait()


def cleanup_networks(networks):
    errors=[]
    for region,(client,state,journal) in networks.items():
        try:cleanup(client,state,journal)
        except Exception as exc:errors.append(f'{region}: {exc}; retain journal {journal}')
    if errors:raise RuntimeError('Network cleanup incomplete: '+'; '.join(errors))


def per_cell_field(args, gateways, cells):
    units=field_units(gateways,cells)
    if not units:raise SystemExit('No measurable cell selected')
    sdk=None
    if args.dry_run and args.quota_vcpus is not None:
        quota,in_use,region,hour=args.quota_vcpus,0.0,args.region or '(assumed)',args.hour_usd
    else:
        session,verify_account,live_prices,capacity,_=aws()
        sdk=session(args.region or os.environ.get('OTB_AWS_REGION') or 'us-east-1');verify_account(sdk)
        prices=live_prices(sdk)
        if args.region:prices=[p for p in prices if p['region']==args.region]
        if not prices:raise SystemExit('No priced region')
        row=prices[0]  # cheapest priced region (or the one named); per-cell keeps the field in one region
        region=row['region'];cap=capacity(sdk,region,INSTANCE_TYPE)
        quota=cap.get('quota_vcpus',0);in_use=quota-cap['slots']*per_cell.VCPUS_PER_BOX if 'quota_vcpus' in cap else quota
        hour=args.hour_usd if args.hour_usd is not None else row['estimated_hour_usd']
    max_boxes=box_budget(quota,in_use,args.quota_margin_vcpus,args.max_boxes)
    batches=per_cell.schedule(units,max_boxes) if max_boxes else []
    if not batches:raise SystemExit(f'No box fits: quota {quota:g} vCPUs, {in_use:g} in use, margin {args.quota_margin_vcpus:g}')
    summary=per_cell.plan_summary(batches,hour)
    print(f'region {region}; {in_use:g} vCPUs already in use; margin {args.quota_margin_vcpus:g}')
    print(per_cell.format_summary(summary,max_boxes,quota))
    if args.candidate_image:print(f'candidate check: {gateways[0]} {", ".join(cells)} with image {args.candidate_image} -> results/candidates/{gateways[0]}/')
    if args.dry_run or args.plan_only:
        if args.dry_run:
            for b in summary['batches']:
                for u in b['units']:print(f"    batch {b['batch']} {u['gateway']}@{','.join(u['cells'])} ~{u['est_s']/60:.0f} min")
        return 0
    return run_per_cell(args,sdk,region,units,max_boxes,cells,gateways,hour)


def run_per_cell(args, sdk, region, units, max_boxes, cells, gateways, hour):
    from bench_placement import capacity
    directory=args.state_dir.resolve()
    if directory==ROOT or ROOT in directory.parents:raise SystemExit('state-dir must be outside the source tree')
    directory.mkdir(parents=True,exist_ok=True)
    stamp=datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ')
    run_id='otb-'+stamp
    engine=(args.rig_dir/'engine-commit').read_text().strip()
    candidate=args.candidate_image
    if candidate:
        out_root=ROOT/'results'/'candidates'/gateways[0]/run_id
    else:
        out_root=ROOT/'results'/'cells'/run_id
    out_root.mkdir(parents=True,exist_ok=True)
    extra={'BENCH_CELL_OUT':str(out_root)}
    if candidate:
        if candidate.startswith('docker-archive:'):
            archive=Path(candidate[len('docker-archive:'):]).resolve()
            if not archive.is_file():raise SystemExit(f'No such archive: {archive}')
            ref=subprocess.check_output(['tar','-xOf',str(archive),'manifest.json'],text=True)
            tags=(json.loads(ref)[0].get('RepoTags') or [])
            if not tags:raise SystemExit('The archive carries no tag; `docker save <name:tag>` so the box can launch it by name')
            extra.update({'BENCH_CANDIDATE_IMAGE':tags[0],'BENCH_CANDIDATE_ARCHIVE':str(archive)})
        else:
            extra['BENCH_CANDIDATE_IMAGE']=candidate
    plan={'run_id':run_id,'source_commit':subprocess.check_output(['git','-C',str(ROOT),'rev-parse','HEAD'],text=True).strip(),'engine_commit':engine,'placement':'per-cell','region':region,'estimated_hour_usd':hour,'max_boxes':max_boxes,'units':units,'candidate_image':candidate,'qualify_band_pct':args.qualify_band_pct,'batches':[]}
    save(directory/'plan.json',plan)
    networks={};children=[]
    install_interrupt()
    remaining=list(units);done_units=[];merged=[]
    try:
        client,image=launch_region(sdk,region)
        plan['images']={region:image};save(directory/'plan.json',plan)
        zone=capacity(sdk,region,INSTANCE_TYPE)['zone']
        journal=directory/f'network-{region}.json'
        state=create(client,run_id,zone,journal);networks[region]=(client,state,journal)
        n=0
        while remaining:
            n+=1
            # Re-read the quota before every batch: other workloads on the account move it.
            cap=capacity(sdk,region,INSTANCE_TYPE)
            quota=cap.get('quota_vcpus',0);in_use=quota-cap['slots']*per_cell.VCPUS_PER_BOX
            slots=box_budget(quota,in_use,args.quota_margin_vcpus,args.max_boxes)
            if slots<1:raise RuntimeError(f'batch {n}: no box fits the quota now ({in_use:g}/{quota:g} vCPUs in use)')
            batch=per_cell.schedule(remaining,slots)[0]
            plan['batches'].append({'batch':n,'units':[f"{u['gateway']}@{','.join(u['cells'])}" for u in batch],'started_at':datetime.datetime.now(datetime.timezone.utc).isoformat()})
            save(directory/'plan.json',plan)
            print(f'batch {n}: {len(batch)} boxes ({len(remaining)-len(batch)} units after it)',flush=True)
            env=child_env(args,directory/region,region,image['ami_id'],state,f'{run_id}-b{n}',extra)
            log=(directory/f'run-{region}-b{n}.log').open('w')
            child=subprocess.Popen(['bash',str(ROOT/'run-on-ec2.sh'),*[f"{u['gateway']}@{','.join(u['cells'])}" for u in batch]],env=env,cwd=ROOT,stdout=log,stderr=subprocess.STDOUT,start_new_session=True)
            log.close();children=[child]
            rc=child.wait()
            if rc!=0:
                # FIRST FAILED BOX JOB ABORTS THE FIELD (run-on-ec2.sh already stopped its own peers).
                print(f'FIELD ABORT: batch {n} failed (rc={rc}); see {directory}/run-{region}-b{n}.log. No further batch is launched.',file=sys.stderr,flush=True)
                return 1
            batch_ids={id(u) for u in batch}
            done_units+=batch;remaining=[u for u in remaining if id(u) not in batch_ids]
            # Merge every gateway whose units are now all measured.
            for g in sorted({u['gateway'] for u in batch}):
                if any(u['gateway']==g for u in remaining):continue
                merged.append(finish_gateway(g,[u for u in done_units if u['gateway']==g],out_root,run_id,candidate,cells,engine,args))
        listing=ROOT/'results'/'box-qualify'/f'measured-files-{run_id}.txt'
        if not candidate:
            listing.parent.mkdir(parents=True,exist_ok=True)
            listing.write_text(''.join(f'{p}\n' for p in merged))
            if merged and subprocess.run([sys.executable,str(ROOT/'history'/'append.py'),*map(str,merged)]).returncode:
                print('WARNING history/append.py failed for the merged snapshots',file=sys.stderr)
                return 1
            print(f'merged {len(merged)} gateway snapshot(s); completion list {listing}')
        return 0
    finally:
        stop_children(children)
        cleanup_networks(networks)


def finish_gateway(gateway, units, out_root, run_id, candidate, cells, engine, args):
    pieces=[]
    for u in units:
        d=out_root/gateway/per_cell.slug(u['cells'])
        found=sorted(d.glob(f'result_{gateway}_*.json'))
        if len(found)!=1:raise RuntimeError(f'{d}: expected exactly one snapshot, found {len(found)}')
        pieces.append(merge_cells.load_piece(found[0]))
    expect=None if candidate else merge_cells.full_grid()
    snap=merge_cells.merge(pieces,expect)
    if not candidate:
        target=ROOT/'results'/'snapshots'/merge_cells.output_name(snap)
        if target.exists():raise RuntimeError(f'refusing to overwrite {target}')
        target.write_text(json.dumps(snap,indent=2)+'\n')
        print(f'{gateway}: merged {len(pieces)} boxes -> {target}',flush=True)
        return target
    from qualification_baseline import calibration_file
    calib=calibration_file(ROOT,args.rig_dir.resolve(),engine,'arm64',INSTANCE_TYPE)
    images=[json.loads((out_root/gateway/per_cell.slug(u['cells'])/'candidate-image.json').read_text()) for u in units]
    if len({json.dumps(i,sort_keys=True) for i in images})!=1:raise RuntimeError(f'candidate boxes ran different images: {images}')
    snap['candidate']=True
    snap['candidate_check']={'run_id':run_id,'image':images[0],'requested':candidate,'cells':cells,
                             'published_image':per_cell.load_manifest(ROOT,gateway)['launch'].get('image'),
                             'engine_pin':engine,'calibration':calib.name if calib else None,
                             'note':'Candidate check: NOT a published result. Never copy into results/snapshots/.'}
    target=out_root/merge_cells.output_name(snap)
    target.write_text(json.dumps(snap,indent=2)+'\n')
    report=out_root/'report'
    subprocess.run([sys.executable,str(ROOT/'lib'/'candidate_report.py'),str(target),'--out',str(report)],check=True)
    print(f'{gateway}: candidate snapshot {target}\n  report {report}.md / {report}.json',flush=True)
    return target


if __name__=='__main__':
    sys.exit(main())
