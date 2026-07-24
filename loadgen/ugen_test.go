// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// Unit tests for the load generator's NUMERIC CORE. ugen's rps/p99/delivered figures ARE every
// published board number, so a regression here inflates/deflates every gateway board-wide with
// nothing else failing. These tests pin the three fixes that shipped with no regression coverage:
//   * rps over MEASURED elapsed, not nominal -d  (audit round-2 M1)
//   * the percentile function + its out-of-bounds index clamp  (audit N2)
//   * the NewRequest-error path increments fail and does not panic  (audit L1)
// plus isContent's empty-placeholder handling (the "text":"" / system_fingerprint:"" regression).
package main

import (
	"net/http/httptest"
	"testing"
)

// ── rps over ELAPSED, not nominal dur (audit M1) ────────────────────────────────────────────────
func TestRpsOverElapsed(t *testing.T) {
	// A tail request in flight past the deadline still completes and is counted in ok, so the real
	// window is LONGER than the nominal -d. Dividing by the (shorter) nominal dur over-counts.
	// 1000 successes over a real 12.5s window is 80 rps; dividing by the nominal 10 would give 100.
	if got := rpsOver(1000, 12.5, 10); got != 80 {
		t.Fatalf("rpsOver(1000,12.5,10)=%d, want 80 (must divide by measured elapsed, not nominal dur)", got)
	}
	// exact division
	if got := rpsOver(500, 10.0, 10); got != 50 {
		t.Fatalf("rpsOver(500,10,10)=%d, want 50", got)
	}
	// truncation toward zero (int64 cast), matching the shipped Printf
	if got := rpsOver(99, 10.0, 10); got != 9 {
		t.Fatalf("rpsOver(99,10,10)=%d, want 9", got)
	}
	// non-positive elapsed must fall back to dur, never divide by zero
	if got := rpsOver(100, 0, 10); got != 10 {
		t.Fatalf("rpsOver(100,0,10)=%d, want 10 (fallback to dur)", got)
	}
	// elapsed and dur both zero -> 0, not a panic / +Inf
	if got := rpsOver(100, 0, 0); got != 0 {
		t.Fatalf("rpsOver(100,0,0)=%d, want 0", got)
	}
}

// ── percentile function + index clamp (audit N2) ────────────────────────────────────────────────
func TestPctBasic(t *testing.T) {
	v := []float64{10, 20, 30, 40, 50}
	// q=0 -> first element
	if got := pct(append([]float64{}, v...), 0); got != 10 {
		t.Fatalf("pct(v,0)=%v, want 10", got)
	}
	// p50 index = int(5*0.5)=2 -> v[2]=30
	if got := pct(append([]float64{}, v...), 0.5); got != 30 {
		t.Fatalf("pct(v,0.5)=%v, want 30", got)
	}
	// p99 index = int(5*0.99)=4 -> v[4]=50
	if got := pct(append([]float64{}, v...), 0.99); got != 50 {
		t.Fatalf("pct(v,0.99)=%v, want 50", got)
	}
}

func TestPctClampAndEmpty(t *testing.T) {
	// empty slice -> 0, never an index panic
	if got := pct(nil, 0.99); got != 0 {
		t.Fatalf("pct(nil,0.99)=%v, want 0", got)
	}
	// q=1.0 would index len(v) (out of bounds) without the clamp; must return the LAST element.
	v := []float64{1, 2, 3}
	if got := pct(append([]float64{}, v...), 1.0); got != 3 {
		t.Fatalf("pct(v,1.0)=%v, want 3 (index must clamp to last element)", got)
	}
	// a rounding overshoot q>1 must also clamp, not panic
	if got := pct(append([]float64{}, v...), 1.5); got != 3 {
		t.Fatalf("pct(v,1.5)=%v, want 3 (overshoot must clamp)", got)
	}
	// pct sorts in place, so an unsorted input still yields the right quantile
	u := []float64{50, 10, 40, 20, 30}
	if got := pct(u, 0.5); got != 30 {
		t.Fatalf("pct(unsorted,0.5)=%v, want 30 (must sort first)", got)
	}
}

// ── isContent: real delta text vs empty-string placeholders (the "text":"" regression) ──────────
func TestIsContent(t *testing.T) {
	cases := []struct {
		line string
		want bool
	}{
		{`data: {"choices":[{"delta":{"content":"hi"}}]}`, true},          // openai delta text
		{`{"type":"content_block_delta","delta":{"text":"tok"}}`, true},    // anthropic delta text
		{`{"content":""}`, false},                                          // empty content placeholder
		{`{"type":"content_block_start","content_block":{"text":""}}`, false}, // anthropic empty text
		// unrelated empty-string field must NOT zero the frame (TensorZero system_fingerprint regression)
		{`{"choices":[{"delta":{"content":"x"}}],"system_fingerprint":""}`, true},
		{`{"id":"chatcmpl","object":"chat.completion.chunk"}`, false}, // no content/text at all
		{`data: [DONE]`, false},
	}
	for _, c := range cases {
		if got := isContent(c.line); got != c.want {
			t.Errorf("isContent(%q)=%v, want %v", c.line, got, c.want)
		}
	}
}

// ── NewRequest error path (audit L1): a malformed URL returns an error, never a nil-req panic ────
func TestBuildRequestErrorPath(t *testing.T) {
	// A control character in the URL makes http.NewRequest fail. The shipped loop treats a non-nil
	// err as a failed request (fail++ ; continue) and must not dereference the nil *Request.
	req, err := buildRequest("http://\x7f\x00bad", `{"model":"m"}`, "sk", "openai", false, nil)
	if err == nil {
		t.Fatal("buildRequest with a malformed URL returned nil error; want an error to drive the fail path")
	}
	if req != nil {
		t.Fatal("buildRequest returned a non-nil req alongside an error")
	}
}

func TestBuildRequestHeaders(t *testing.T) {
	srv := httptest.NewServer(nil)
	defer srv.Close()
	// openai shape: bearer only, no anthropic carriers
	req, err := buildRequest(srv.URL, `{"model":"m"}`, "sk-abc", "openai", false, []string{"X-Extra: v"})
	if err != nil {
		t.Fatalf("buildRequest openai: unexpected error %v", err)
	}
	if got := req.Header.Get("authorization"); got != "Bearer sk-abc" {
		t.Errorf("authorization=%q, want %q", got, "Bearer sk-abc")
	}
	if req.Header.Get("x-api-key") != "" {
		t.Error("openai shape must not set x-api-key")
	}
	if got := req.Header.Get("X-Extra"); got != "v" {
		t.Errorf("extra header X-Extra=%q, want v", got)
	}
	// anthropic shape sends BOTH auth carriers + the version header
	areq, err := buildRequest(srv.URL, `{"model":"m","max_tokens":1}`, "sk-xyz", "anthropic", true, nil)
	if err != nil {
		t.Fatalf("buildRequest anthropic: unexpected error %v", err)
	}
	if got := areq.Header.Get("x-api-key"); got != "sk-xyz" {
		t.Errorf("anthropic x-api-key=%q, want sk-xyz", got)
	}
	if got := areq.Header.Get("anthropic-version"); got != "2023-06-01" {
		t.Errorf("anthropic-version=%q, want 2023-06-01", got)
	}
	if got := areq.Header.Get("accept"); got != "text/event-stream" {
		t.Errorf("stream=true must set accept=text/event-stream, got %q", got)
	}
}
