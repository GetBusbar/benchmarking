#!/usr/bin/env python3
"""An optional TIGHTER box-qualification band, applied by the orchestrator to a harvested snapshot.

The engine's own band (QUALIFY_BAND_PCT = 20 in engine/src/suite.rs) is part of the frozen instrument
and does not change here. This check reads the verdict the engine recorded (rig.box_qualify) and
additionally refuses a box whose drift is below -BAND percent. One-sided, like the engine's: a box
cannot randomly get faster, so only degradation counts. Unset means no extra check (the default).
"""
import json
from pathlib import Path
import sys


def check(snapshot, band_pct):
    """Return None when the box is inside `band_pct`, else the reason it is not."""
    q = ((snapshot.get('rig') or {}).get('box_qualify')) or {}
    drift = q.get('drift_pct')
    if q.get('outcome') not in ('pass', 'seed'):
        return f'engine outcome {q.get("outcome")!r}'
    if not isinstance(drift, (int, float)) or isinstance(drift, bool):
        return f'no drift recorded (outcome {q.get("outcome")!r}); a tighter band cannot judge an absent reading'
    if drift < -band_pct:
        return f'drift {drift:+.2f}% is outside the -{band_pct:g}% band (observed {q.get("observed_rps")} vs baseline {q.get("baseline_rps")} req/s)'
    return None


def main(argv):
    band = float(argv[1])
    failed = 0
    for path in argv[2:]:
        why = check(json.loads(Path(path).read_text()), band)
        if why:
            print(f'BOX OUTSIDE TIGHTER QUALIFICATION BAND: {Path(path).name}: {why}', file=sys.stderr)
            failed = 1
        else:
            print(f'box band: {Path(path).name} within -{band:g}%')
    return failed


if __name__ == '__main__':
    sys.exit(main(sys.argv))
