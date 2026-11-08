// SPDX-License-Identifier: Apache-2.0
import { projectMeasurements, compareMeasurements, compactRecords } from './public-data.mjs';
import { sealMatrixCellsInPlace } from './seal-matrix.mjs';
import { applyRunReview } from './run-reviews.mjs';
import { esc } from './render.mjs';
export const PROJECTOR = 'historical-matrix-v1';
export function expandHistorical(bundle) {
  return bundle.measurements.map(m => ({...m,test:{...m.test,definitions:Object.fromEntries(Object.entries(m.test.definition_ids || {}).map(([k,id])=>[k,bundle.definitions[id]]))},metrics:Object.fromEntries(Object.entries(m.metrics).map(([k,v])=>[k,{...v,definition:bundle.definitions[v.definition_id] ?? null}]))}));
}
export function historicalMeasurements(snapshot, run) {
  if (!snapshot.matrix) return [];
  const matrix = structuredClone(applyRunReview(snapshot, run.review).matrix);
  if (!matrix.upstreams && matrix.cells) matrix.upstreams = { [matrix.upstream_shape || 'openai']: { cells: matrix.cells } };
  matrix.measured_at ??= snapshot.measured_at;
  matrix.build ??= snapshot.build;
  sealMatrixCellsInPlace(matrix);
  return projectMeasurements({ key: snapshot.gateway, matrix, snapshot_file: run.id }, new Map([[run.id, { ...run, snapshot }]])).map(m => ({ ...m, test: { ...m.test, configuration_sha256: run.configuration_sha256 ?? null } }));
}
export function compareRuns(left, right, metric, bound) {
  const result = compareMeasurements([{ ...left, gateway_id: 'earlier' }, { ...right, gateway_id: 'later' }], {
    a: 'earlier', b: 'later', ingress: left.test.ingress_protocol, egress: left.test.upstream_protocol, metric, bound,
  });
  const a = left.test.configuration_sha256, b = right.test.configuration_sha256;
  if (!a || !b || a !== b) {
    result.comparability.reasons.push(!a || !b ? 'missing_configuration_identity' : 'different_configuration_identity');
    result.comparability.status = a && b && a !== b ? 'incomparable' : 'insufficient_evidence';
    result.results.gateway_b_over_a = null;
  }
  return result;
}
export const TREND_TOLERANCE_PERCENT = 5;
export function trendIndicator(before, after, direction, comparable = true) {
  const base = { tolerance_percent: TREND_TOLERANCE_PERCENT, change_percent: null };
  if (!comparable || !direction || !Number.isFinite(before) || !Number.isFinite(after) || before < 0 || after < 0)
    return { ...base, state: 'not_comparable', symbol: '·', label: 'Not comparable' };
  if (before === 0 && after !== 0) return { ...base, state: 'not_comparable', symbol: '·', label: 'No percentage baseline' };
  const change = before === 0 ? 0 : (after-before)/before*100;
  if (Math.abs(after-before) <= Math.abs(before)*0.05 + Number.EPSILON*Math.max(before,after,1))
    return { ...base, change_percent: change, state: 'flat', symbol: '—', label: 'Roughly unchanged' };
  const improved = direction === 'higher' ? after > before : after < before;
  return { ...base, change_percent: change, state: improved ? 'improved' : 'regressed', symbol: improved ? '↑' : '↓', label: improved ? 'Improved' : 'Regressed' };
}
export function metricDirection(metric) {
  if (['throughput_rps','streams_sustained','streams_sustained_fps','rps_per_dollar','rps_per_cpu_core'].includes(metric)) return 'higher';
  if (/latency|ttft|gap|rss_mib$|cost_per_|cpu_us|memory_growth/.test(metric)) return 'lower';
  return null; // Do not invent a desirable direction for categorical or unknown metrics.
}
const trendHtml = t => `<span class="history-trend trend-${t?.state || 'not_comparable'}">${esc(t?.symbol || '·')} ${esc(t?.label || 'No comparable predecessor')}${t?.change_percent == null ? '' : ` (${t.change_percent > 0 ? '+' : ''}${t.change_percent.toFixed(2)}%)`}</span>${t?.reasons?.length ? `<small class="history-comparison-reasons">${t.reasons.map(r => esc(r.replaceAll('_', ' '))).join('; ')}</small>` : ''}`;
export function historyPoints(records, q) {
  const ordered = records.filter(m => m.test.ingress_protocol === q.ingress && m.test.upstream_protocol === q.egress).sort((a,b) => (Date.parse(a.measured_at)||0)-(Date.parse(b.measured_at)||0));
  return ordered.map((m,index) => {
    const metric = q.metric === 'throughput_rps' ? m.frontier.find(f => f.bound_ms === (q.bound === 'none' ? null : Number(q.bound || 10)))?.rps : m.metrics[q.metric];
    const previous = ordered.slice(0,index).reverse().find(p => Number.isFinite(Date.parse(p.measured_at)) && Date.parse(p.measured_at) < Date.parse(m.measured_at) && compareRuns(p,m,q.metric,q.bound).comparability.status === 'comparable');
    const priorMetric = previous && (q.metric === 'throughput_rps' ? previous.frontier.find(f=>f.bound_ms === (q.bound === 'none' ? null : Number(q.bound || 10)))?.rps : previous.metrics[q.metric]);
    const trend = trendIndicator(priorMetric?.value,metric?.value,metricDirection(q.metric),!!previous);
    const latestEarlier = ordered.slice(0,index).reverse().find(p => Date.parse(p.measured_at) < Date.parse(m.measured_at));
    const reasons = previous ? [] : latestEarlier ? compareRuns(latestEarlier,m,q.metric,q.bound).comparability.reasons : ['no_earlier_measurement'];
    if (!previous) trend.label = reasons.includes('different_harness_commit') ? 'Engine changed' : reasons.includes('no_earlier_measurement') ? 'First recorded run' : 'Comparison unavailable';
    return { trend: { ...trend, reasons, previous_run_id: previous?.run_id ?? null }, run_id: m.run_id, measurement_id: m.measurement_id, gateway_id: m.gateway_id, gateway_build: m.gateway_build, measured_at: m.measured_at,
      engine: m.test.harness_commit, metric: q.metric, value: metric?.value ?? null, state: metric?.state ?? 'not_measured', reason: metric?.reason ?? null,
      definition: metric?.definition ?? null, provenance: m.provenance };
  });
}
// Publish only supported changes for the current measurement; absent comparisons add no UI noise.
export function currentTrends(records, current) {
  const trends = {};
  for (const m of current) {
    const earlier = records.filter(p => p.test.ingress_protocol === m.test.ingress_protocol && p.test.upstream_protocol === m.test.upstream_protocol && Date.parse(p.measured_at) < Date.parse(m.measured_at)).sort((a,b)=>Date.parse(b.measured_at)-Date.parse(a.measured_at));
    const queries = Object.keys(m.metrics).map(metric=>({metric})).concat(m.frontier.map(f=>({metric:'throughput_rps',bound:f.bound_ms == null ? 'none' : String(f.bound_ms)})));
    for (const q of queries) {
      const direction = metricDirection(q.metric);
      if (!direction) continue;
      const before = earlier.find(p=>compareRuns(p,m,q.metric,q.bound).comparability.status === 'comparable');
      if (!before) continue;
      const value = r => q.metric === 'throughput_rps' ? r.frontier.find(f=>f.bound_ms === (q.bound === 'none' ? null : Number(q.bound)))?.rps?.value : r.metrics[q.metric]?.value;
      const trend = trendIndicator(value(before),value(m),direction);
      if (trend.state === 'not_comparable') continue;
      const key = `${m.test.ingress_protocol}/${m.test.upstream_protocol}/${q.metric}${q.bound == null ? '' : '/' + q.bound}`;
      trends[key] = {...trend, previous_run_id:before.run_id,current_run_id:m.run_id,previous_measured_at:before.measured_at,metric:q.metric,ingress:m.test.ingress_protocol,egress:m.test.upstream_protocol,bound:q.bound};
    }
  }
  return trends;
}
export function historyArticle(family, page, q, comparisons = []) {
  const link = `/gateways/${family}`;
  const options = values => values.map(v => `<option value="${esc(v)}"${v === q.metric ? ' selected' : ''}>${esc(v.replaceAll('_', ' '))}</option>`).join('');
  const points = page.points;
  return `<h1>${esc(family)} history</h1><nav aria-label="Gateway"><a href="${link}">Overview</a> · <strong>History</strong> · <a href="${link}/evidence">Evidence</a></nav>
  <p>Every retained run, including incomplete and unavailable evidence. Values describe the recorded local-mock configuration. Missing readings are never zero.</p>
  <form method="get" class="history-controls"><label>Ingress <input name="ingress" value="${esc(q.ingress)}" list="protocols"></label> <label>Upstream <input name="egress" value="${esc(q.egress)}" list="protocols"></label><datalist id="protocols">${['openai','openai-responses','anthropic','gemini','cohere','bedrock'].map(p=>`<option value="${p}">`).join('')}</datalist> <label>Metric <select name="metric">${options(page.metrics)}</select></label> <label>Throughput p99 bound <select name="bound">${['1','5','10','50','100','none'].map(b=>`<option value="${b}"${b === (q.bound || '10') ? ' selected' : ''}>${b === 'none' ? 'Unbounded' : `${b} ms`}</option>`).join('')}</select></label> <label>From <input type="date" name="from" value="${esc(q.from || '')}"></label> <label>Through <input type="date" name="to" value="${esc(q.to || '')}"></label> <button>Show runs</button></form>
  <p>↑ Improved · ↓ Regressed · — Roughly unchanged (within ±5%). This is a display tolerance, not a statistical margin of error. Trends use the preceding comparable run; unknown evidence is neutral.</p>
  <p>${page.total} retained runs. Catalog <code>${esc(page.catalog_id)}</code>.</p>
  <div class="table-scroll"><table><thead><tr><th>Measured</th><th>Build</th><th>Engine</th><th>Result</th><th>Trend</th><th>Field placement</th><th>Evidence</th></tr></thead><tbody>${points.map(p=>`<tr><td>${esc(p.measured_at || 'Undated')}</td><td>${esc(p.gateway_build || 'Not recorded')}</td><td>${esc(p.engine?.slice(0,12) || 'Not recorded')}</td><td>${p.value == null ? 'n/a' : esc(p.value)} — ${esc(p.state)}</td><td>${trendHtml(p.trend)}</td><td>${p.placements?.length ? p.placements.map(r=>`<a href="/api/v1/fields/${esc(r.field_id)}">${r.rank} of ${r.eligible} eligible / ${r.rostered} rostered</a> (${esc(r.field_name)})`).join("<br>") : "Not ranked in a retained field"}</td><td><a href="${esc(p.provenance?.run_url || '#')}">Run evidence</a></td></tr>`).join('')}</tbody></table></div>
  ${page.next ? `<p><a href="${esc(page.next)}">Next page of runs</a></p>` : ''}
  <details><summary>Compare any two runs across recorded metrics</summary><form method="get" class="history-controls"><input type="hidden" name="ingress" value="${esc(q.ingress)}"><input type="hidden" name="egress" value="${esc(q.egress)}"><label>Earlier run <select name="a">${points.map(p=>`<option value="${esc(p.run_id)}">${esc(p.measured_at)} ${esc(p.gateway_build)}</option>`).join('')}</select></label> <label>Later run <select name="b">${points.map(p=>`<option value="${esc(p.run_id)}">${esc(p.measured_at)} ${esc(p.gateway_build)}</option>`).join('')}</select></label> <button>Compare</button></form></details>
  ${comparisons.length ? `<h2>Selected run comparison</h2><table><thead><tr><th>Metric</th><th>Earlier</th><th>Later</th><th>Comparison</th></tr></thead><tbody>${comparisons.map(c=>`<tr><td>${esc(c.metric)}</td><td>${esc(c.before ?? 'n/a')}</td><td>${esc(c.after ?? 'n/a')}</td><td>${trendHtml(c.trend)} · ${esc(c.status)}${c.change_percent == null ? '' : `: ${c.change_percent.toFixed(2)}%`}; ${esc(c.reasons.join(', '))}</td></tr>`).join('')}</tbody></table>` : ''}
  <p><a href="/api/v1/history?family=${esc(family)}">Bounded history JSON</a> · <a href="/gateways/runs">All gateway runs</a></p>`;
}
export function historicalComparison(left, right) {
  const metrics = [...new Set([...Object.keys(left.metrics), ...Object.keys(right.metrics)])].map(metric => ({metric}));
  const bounds = [...new Set([...(left.frontier || []), ...(right.frontier || [])].map(f => f.bound_ms))];
  metrics.push(...bounds.map(bound => ({metric: 'throughput_rps', bound: bound == null ? 'none' : String(bound)})));
  return metrics.map(({metric, bound}) => {
    const c = compareRuns(left, right, metric, bound);
    const reading = record => metric === 'throughput_rps'
      ? record.frontier?.find(f => f.bound_ms === (bound === 'none' ? null : Number(bound)))?.rps?.value ?? null
      : record.metrics[metric]?.value ?? null;
    const before = reading(left), after = reading(right);
    return { trend: trendIndicator(before, after, metricDirection(metric), c.comparability.status === "comparable"),
      metric: bound == null ? metric : `${metric} (p99 ${bound === 'none' ? 'unbounded' : `≤ ${bound} ms`})`,
      before, after, status: c.comparability.status, reasons: c.comparability.reasons,
      change_percent: c.comparability.status === 'comparable' && before !== null && after !== null && before !== 0 ? (after-before)/Math.abs(before)*100 : null };
  });
}

export function fieldRankings(collection, q) {
  const rows=collection.measurements.filter(m=>m.test.ingress_protocol===q.ingress && m.test.upstream_protocol===q.egress);
  const direction=metricDirection(q.metric);
  const value=m=>q.metric==='throughput_rps' ? m.frontier.find(f=>f.bound_ms===(q.bound==='none'?null:Number(q.bound||10)))?.rps?.value : m.metrics[q.metric]?.value;
  const groups=[],excluded=[];
  for(const row of rows) {
    const self=compareMeasurements([{...row,gateway_id:'a'},{...row,gateway_id:'b'}],{a:'a',b:'b',ingress:q.ingress,egress:q.egress,metric:q.metric,bound:q.bound});
    if(!direction || self.comparability.status!=='comparable') {excluded.push({gateway_id:row.gateway_id,reasons:direction?self.comparability.reasons:['metric_has_no_ranking_direction']});continue;}
    let group=groups.find(group=>compareMeasurements([{...group[0],gateway_id:'a'},{...row,gateway_id:'b'}],{a:'a',b:'b',ingress:q.ingress,egress:q.egress,metric:q.metric,bound:q.bound}).comparability.status==='comparable');
    if(!group){group=[];groups.push(group);}group.push(row);
  }
  return {metric:q.metric,ingress:q.ingress,egress:q.egress,direction,rostered:new Set(collection.gateways?.map(g=>g.id) || collection.measurements.map(m=>m.gateway_id)).size,excluded,
    groups:groups.map((group,index)=>({group:index+1,eligible:group.length,rankings:group.sort((a,b)=>(value(a)-value(b))*(direction==='lower'?1:-1)).map((m,idx,sorted)=>({gateway_id:m.gateway_id,value:value(m),rank:sorted.findIndex(other=>value(other)===value(m))+1,measurement_id:m.measurement_id,run_id:m.run_id}))})),
    interpretation:'Placement within this immutable field and compatible group only. Different fields or engines do not imply a rank improvement. Values within the 5% trend tolerance are not necessarily exact ranking ties.'};
}

// Rank movement needs the same complete roster and a compatible field-wide cohort.
export function rankChanges(before, after, q) {
  const roster = c => [...new Set(c.gateways?.map(g=>g.id) || c.measurements.map(m=>m.gateway_id))].sort();
  const ids = roster(after);
  if (!ids.length || JSON.stringify(ids) !== JSON.stringify(roster(before))) return {};
  const complete = c => ids.every(id=>c.measurements.some(m=>m.gateway_id===id));
  if (!complete(before) || !complete(after)) return {};
  const a = fieldRankings(before,q), b = fieldRankings(after,q);
  if (a.groups.length !== 1 || b.groups.length !== 1) return {};
  const old = a.groups[0].rankings, fresh = b.groups[0].rankings;
  const members = rows => rows.map(r=>r.gateway_id).sort();
  if (JSON.stringify(members(old)) !== JSON.stringify(members(fresh))) return {};
  for (const id of members(fresh)) {
    const selected = c => c.measurements.find(m=>m.gateway_id===id && m.test.ingress_protocol===q.ingress && m.test.upstream_protocol===q.egress);
    if (compareMeasurements([{...selected(before),gateway_id:'a'},{...selected(after),gateway_id:'b'}],{...q,a:'a',b:'b'}).comparability.status !== 'comparable') return {};
  }
  return Object.fromEntries(fresh.map(r=>{
    const prior=old.find(p=>p.gateway_id===r.gateway_id);
    return [r.gateway_id,{change:prior.rank-r.rank,before:prior.rank,after:r.rank,previous_run_id:prior.run_id}];
  }));
}
