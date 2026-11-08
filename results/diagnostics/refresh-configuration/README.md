# Configuration diagnostics retained as evidence

The first Bifrost 2.5.0 attempt on 2026-11-08 passed machine qualification but its inference probes returned `virtual_key_required`. This is a benchmark configuration failure, not a product performance result. Its immutable snapshot is retained here, excluded from the current refresh board. The release requires an explicit virtual key and permission to reach the private local mock network; the corrected configuration is rerun before publication.
