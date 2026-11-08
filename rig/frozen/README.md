# Frozen instruments

Each commit directory retains the exact engine and mock executables used by a field refresh, gzip-compressed without a variable timestamp. Never replace existing bytes. The SHA256SUMS file hashes the decompressed executables; engine-commit names the source instrument.

Extract into a task directory, copy engine-commit and SHA256SUMS beside the binaries, and run:

```
python3 lib/verify_rig.py /path/to/rig <engine-commit> arm64
```

Pass that directory as BENCH_RIG_DIR to run-on-ec2.sh. Update one gateway manifest and rerun only that gateway to preserve the instrument used by the rest of the field. A different engine requires another field-wide experiment.

AWS operations use the SDK adapter in lib/bench-aws. Install lib/aws-sdk-requirements.txt into a virtual environment and set OTB_AWS_PYTHON to its Python executable.

Set OTB_AWS_ACCOUNT_ID explicitly. Select OTB_AWS_PROFILE for a named SDK profile, use standard AWS environment credentials or web/container role credentials, or opt into the machine role with OTB_AWS_USE_INSTANCE_ROLE=1. No AWS CLI is invoked. An optional OTB_AWS_KEYCHAIN_SERVICE reads a local macOS Keychain JSON object containing account_id, region_name, aws_access_key_id, aws_secret_access_key and optionally aws_session_token. Never commit this JSON or credential values.

Use OTB_AWS_REGION to select a region. The SDK verifies the actual STS account before resource mutations. Do not put credential values in command arguments, tracked environment files or result manifests.
