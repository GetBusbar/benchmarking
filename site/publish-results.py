#!/usr/bin/env python3
"""Publish only completed, qualified refresh snapshots listed by the runner."""
import argparse
from datetime import datetime
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import time


def collect(source, publication, measured_files):
    active=json.loads((publication/'results/active-field.json').read_text())
    selected=[]
    for listing in measured_files:
        if not listing.exists():continue
        for line in listing.read_text().splitlines():
            path=Path(line).resolve()
            if path.parent != (source/'results/snapshots').resolve():raise ValueError('Completion list points outside the snapshot directory')
            record=json.loads(path.read_text());rig=record.get('rig') or {}
            if (rig.get('engine') or {}).get('commit') != active['engine_commit']:continue
            if datetime.fromisoformat(record['measured_at'].replace('Z','+00:00')) < datetime.fromisoformat(active['started_at'].replace('Z','+00:00')):continue
            if record.get('gateway') not in active['gateways']:raise ValueError('Unexpected entrant')
            if (rig.get('box_qualify') or {}).get('outcome') not in ('pass','seed'):raise ValueError('Snapshot did not qualify')
            if not record.get('matrix'):raise ValueError('Completed snapshot has no matrix')
            selected.append((path,hashlib.sha256(path.read_bytes()).hexdigest()))
    return sorted(set(selected))


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--source',type=Path,required=True)
    p.add_argument('--publication',type=Path,required=True)
    p.add_argument('--state',type=Path,required=True)
    p.add_argument('--measured-files',type=Path,nargs='+',required=True)
    p.add_argument('--watch',action='store_true')
    p.add_argument('command',nargs=argparse.REMAINDER)
    args=p.parse_args();command=args.command
    if command and command[0]=='--':command=command[1:]
    if not command:p.error('Supply a publication command after --')
    source=args.source.resolve();publication=args.publication.resolve()
    args.state.parent.mkdir(parents=True,exist_ok=True)
    done=set(json.loads(args.state.read_text())) if args.state.exists() else set()
    while True:
        candidates=[(path,digest) for path,digest in collect(source,publication,args.measured_files) if digest not in done]
        if candidates:
            for path,digest in candidates:
                target=publication/'results/snapshots'/path.name;target.parent.mkdir(parents=True,exist_ok=True)
                if target.exists() and hashlib.sha256(target.read_bytes()).hexdigest()!=digest:raise ValueError('Immutable snapshot filename collision')
                shutil.copyfile(path,target)
            result=subprocess.run(command,cwd=publication)
            if result.returncode:
                # Leave evidence on disk for diagnosis, but never mark it published.
                if not args.watch:return result.returncode
                print('Publication failed; retained snapshots will be retried',flush=True)
            else:
                done.update(digest for _,digest in candidates)
                temporary=args.state.with_suffix('.writing');temporary.write_text(json.dumps(sorted(done))+'\n');os.replace(temporary,args.state)
                print(f'Published {len(candidates)} new validated snapshots',flush=True)
        if not args.watch:return 0
        time.sleep(30)

if __name__=='__main__':raise SystemExit(main())
