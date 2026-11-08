# Frozen instruments

Each commit directory retains the exact engine and mock executables used by a field refresh, gzip-compressed without a variable timestamp. Never replace existing bytes. The SHA256SUMS file hashes the decompressed executables; engine-commit names the source instrument.

Extract into a task directory, copy engine-commit and SHA256SUMS beside the binaries, and run:

```
python3 lib/verify_rig.py /path/to/rig <engine-commit> arm64
```

Pass that directory as BENCH_RIG_DIR to run-on-ec2.sh. Update one gateway manifest and rerun only that gateway to preserve the instrument used by the rest of the field. A different engine requires another field-wide experiment.

AWS operations use the SDK adapter in lib/bench-aws. Install lib/aws-sdk-requirements.txt into a task virtual environment and set OTB_AWS_PYTHON to its Python executable. Credentials are read from the macOS Keychain service onthebench-aws-personal; the adapter verifies account 457667483187 before launching or terminating resources. Credentials never belong in this repository.
