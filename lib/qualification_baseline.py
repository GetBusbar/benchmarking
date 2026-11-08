#!/usr/bin/env python3
"""Resolve qualification history only for the exact instrument being launched."""
import hashlib
import json
from pathlib import Path
import statistics
import sys


def median_for(records, engine, mock_sha):
    values=[]
    for record in records:
        rig=record.get('rig') or {}
        if (rig.get('engine') or {}).get('commit') != engine or (rig.get('mock') or {}).get('sha256') != mock_sha:
            continue
        q=rig.get('box_qualify') or {};value=q.get('observed_rps')
        if q.get('outcome') in ('pass','seed') and isinstance(value,(int,float)) and not isinstance(value,bool) and value>0:
            values.append(value)
    return statistics.median(values) if values else None


def resolve(root, rig_dir, engine, arch, instance_type):
    mock=hashlib.sha256((rig_dir/f'mock-{arch}').read_bytes()).hexdigest()
    loadgen=hashlib.sha256((rig_dir/f'otb-{arch}').read_bytes()).hexdigest()
    # A frozen control calibration takes precedence over moving field history.
    for path in sorted((root/'results/calibrations').glob('*.json')):
        c=json.loads(path.read_text())
        if (c.get('engine_commit'),c.get('mock_sha256'),c.get('load_generator_sha256'),c.get('instance_type')) != (engine,mock,loadgen,instance_type):continue
        observations=c['observations']
        if not observations or any(o.get('fail') != 0 or not isinstance(o.get('rps'),(int,float)) or o['rps']<=0 for o in observations):raise ValueError('Invalid calibration observations')
        value=statistics.median(o['rps'] for o in observations)
        print(f'Qualification calibration: {path.name}; exact engine and binary hashes; {len(observations)} clean control windows',file=sys.stderr)
        return value
    records=[]
    for path in (root/'results/snapshots').glob('result_*.json'):
        try:records.append(json.loads(path.read_text()))
        except (ValueError,OSError):continue
    return median_for(records,engine,mock)

if __name__=='__main__':
    value=resolve(Path(sys.argv[1]),Path(sys.argv[2]),*sys.argv[3:])
    if value is not None:print(f'{value:.0f}',end='')
