#!/usr/bin/env python3
"""Per-cell placement: expand gateways into box units and schedule them in quota-sized batches.

A UNIT is one fresh box. Every cell the manifest lets the engine probe (declared '1', or undeclared)
gets a unit of its own. Cells the engine never probes - a declared '0' (not_configurable) or a rig
`untestable` cell - are not worth a box: the engine emits them from the manifest in microseconds,
without sending a request. They ride on the gateway's FIRST measured unit (OTB_CELLS names them
beside the measured cell), so the merged result still carries the engine's own record of them, not
one this tool wrote. The same precedence as engine/src/run.rs run_grid_cells: untestable is checked
before the declared matrix.

Nothing here talks to AWS. run-field.py supplies the box budget (from Service Quotas) and prices.
"""
import json
from pathlib import Path
import statistics

# engine/src/ingress.rs Dialect::ALL - the axis order of a manifest `matrix` both ways.
DIALECTS = ['openai', 'openai-responses', 'anthropic', 'gemini', 'cohere', 'bedrock']
VCPUS_PER_BOX = 16                 # m7g.4xlarge; run-on-ec2.sh refuses any other size.
BOX_OVERHEAD_S = 10 * 60           # boot, deps, rig fetch, image pull, qualification, harvest
DEFAULT_CELL_S = 25 * 60           # no previous timing for this cell or gateway


def cell_name(ingress, egress):
    return f'{ingress}>{egress}'


def slug(cells):
    """Filesystem/tag-safe name of a unit: its first (measured) cell."""
    return cells[0].replace('>', '--')


def classify(manifest):
    """Return (measured, unsupported): cell names in the engine's walk order (egress outer)."""
    matrix = manifest.get('matrix') or []
    untestable = set(manifest.get('untestable') or [])
    measured, unsupported = [], []
    for e_i, egress in enumerate(DIALECTS):
        for i_i, ingress in enumerate(DIALECTS):
            name = cell_name(ingress, egress)
            if f'{ingress}/{egress}' in untestable:
                unsupported.append(name)
                continue
            row = matrix[i_i] if i_i < len(matrix) else None
            declared = None if row is None or e_i >= len(row) else row[e_i] == '1'
            (unsupported if declared is False else measured).append(name)
    return measured, unsupported


def units_for(gateway, manifest, only=None):
    """Box units for one gateway. `only` narrows to named cells (a candidate check)."""
    measured, unsupported = classify(manifest)
    if only:
        unknown = [c for c in only if c not in measured and c not in unsupported]
        if unknown:
            raise ValueError(f'{gateway}: not cells of the 6x6 grid: {", ".join(unknown)}')
        skipped = [c for c in only if c in unsupported]
        if skipped:
            raise ValueError(f'{gateway}: {", ".join(skipped)} is declared unsupported/untestable; '
                             'the engine would not measure it, so a box would buy nothing')
        return [{'gateway': gateway, 'cells': [c]} for c in measured if c in only]
    units = [{'gateway': gateway, 'cells': [c]} for c in measured]
    if units:
        units[0]['cells'] += unsupported           # never probed: costs no box of its own
    elif unsupported:
        units.append({'gateway': gateway, 'cells': unsupported})
    return units


def load_manifest(root, gateway):
    return json.loads((Path(root) / 'gateways' / gateway / 'definition.json').read_text())


def previous_timings(root, gateway):
    """Per-cell wall seconds from this gateway's newest snapshot (the sum of the engine's timings_s)."""
    snaps = sorted((Path(root) / 'results' / 'snapshots').glob(f'result_{gateway}_*.json'))
    for path in reversed(snaps):
        try:
            snap = json.loads(path.read_text())
        except (OSError, ValueError):
            continue
        out = {}
        for egress, up in ((snap.get('matrix') or {}).get('upstreams') or {}).items():
            for ingress, cell in ((up or {}).get('cells') or {}).items():
                t = cell.get('timings_s') if isinstance(cell, dict) else None
                if isinstance(t, dict):
                    total = sum(v for v in t.values() if isinstance(v, (int, float)))
                    if total > 0:
                        out[cell_name(ingress, egress)] = total
        if out:
            return out
    return {}


def estimate(units, timings):
    """Attach `est_s` (measurement only, without box overhead) to each unit."""
    for u in units:
        t = timings.get(u['gateway'], {})
        fallback = statistics.median(t.values()) if t else DEFAULT_CELL_S
        u['est_s'] = t.get(u['cells'][0], fallback)
    return units


def schedule(units, max_boxes):
    """Batches of at most `max_boxes` units, keeping each gateway's cells together.

    A gateway is one item; one larger than a batch is cut into batch-sized chunks plus a remainder,
    longest-expected cells first. Items are packed first-fit-decreasing, so a batch is not left
    half-empty because the next gateway does not fit whole. Each batch is one barrier: the next starts
    when every box of this one is torn down.
    """
    if max_boxes < 1:
        raise ValueError('No box fits in the available vCPU quota')
    by_gw = {}
    for u in units:
        by_gw.setdefault(u['gateway'], []).append(u)
    items = []
    for gateway, group in by_gw.items():
        group = sorted(group, key=lambda u: -u.get('est_s', 0))
        items += [group[i:i + max_boxes] for i in range(0, len(group), max_boxes)]
    order = {g: i for i, g in enumerate(by_gw)}
    items.sort(key=lambda it: (-len(it), order[it[0]['gateway']]))
    batches = []
    for item in items:
        for batch in batches:
            if len(batch) + len(item) <= max_boxes:
                batch.extend(item)
                break
        else:
            batches.append(list(item))
    return batches


def plan_summary(batches, hour_usd, overhead_s=BOX_OVERHEAD_S):
    """Wall time and cost of a schedule. Cost counts every box for its own lifetime only."""
    rows, wall, box_s = [], 0.0, 0.0
    for n, batch in enumerate(batches, 1):
        longest = max(u['est_s'] for u in batch) + overhead_s
        wall += longest
        box_s += sum(u['est_s'] + overhead_s for u in batch)
        rows.append({'batch': n, 'boxes': len(batch), 'gateways': sorted({u['gateway'] for u in batch}),
                     'cells': sum(len(u['cells']) for u in batch), 'wall_s': longest,
                     'units': [{'gateway': u['gateway'], 'cells': u['cells'], 'est_s': u['est_s']} for u in batch]})
    return {'batches': rows, 'boxes': sum(len(b) for b in batches), 'wall_s': wall,
            'box_hours': box_s / 3600, 'hour_usd': hour_usd,
            'cost_usd': None if hour_usd is None else box_s / 3600 * hour_usd,
            'overhead_s_per_box': overhead_s}


def format_summary(summary, max_boxes, quota_vcpus):
    lines = [f'per-cell schedule: quota {quota_vcpus:g} vCPUs -> at most {max_boxes} concurrent m7g.4xlarge boxes']
    for b in summary['batches']:
        lines.append(f"  batch {b['batch']:>2}: {b['boxes']:>3} boxes, {b['cells']:>3} cells, "
                     f"~{b['wall_s'] / 60:.0f} min - {', '.join(b['gateways'])}")
    cost = 'unpriced' if summary['cost_usd'] is None else f"${summary['cost_usd']:.2f} at ${summary['hour_usd']:.4f}/box-hour"
    lines.append(f"  total: {summary['boxes']} boxes, {len(summary['batches'])} batches, "
                 f"~{summary['wall_s'] / 3600:.1f} h wall, {summary['box_hours']:.1f} box-hours, {cost} "
                 f"(includes ~{summary['overhead_s_per_box'] / 60:.0f} min boot/qualify/harvest per box)")
    return '\n'.join(lines)
