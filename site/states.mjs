// SPDX-License-Identifier: Apache-2.0
export const METRIC_STATES = ["valid", "below_resolution", "not_measured", "not_configured", "untestable", "rig_limited", "search_exhausted", "harness_error", "not_served", "mock_bound", "unverifiable"];
export const CELL_STATES = ["served", "not_configured", "not_declared", "untestable", "unprobed_auth", "not_served", "not_verified", "not_measured"];
export function canonicalCellState(cell) {
  if (cell.perf?.egress_reverified === false) return "not_served";
  if (cell.served === true) return "served";
  if (cell.served === false || cell.served === "failed") return "not_served";
  if (cell.served === "not_configurable") return "not_declared";
  if (cell.served === "not_probed") return "not_verified";
  return CELL_STATES.includes(cell.served) ? cell.served : "not_verified";
}
