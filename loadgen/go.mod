// Self-contained module for the load generator (the component whose rps/p99 IS every published
// board number). Kept dependency-free (stdlib only) so `go test ./loadgen/...` needs no network.
module github.com/GetBusbar/benchmarking/loadgen

go 1.21
