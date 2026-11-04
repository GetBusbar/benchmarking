#!/usr/bin/env python3
"""Exercise run-local security-group creation without making AWS calls."""
import pathlib
import subprocess
import unittest

SOURCE = (pathlib.Path(__file__).resolve().parents[1] / "run-on-ec2.sh").read_text()
BLOCK = SOURCE.split("# Each invocation owns its security group", 1)[1].split("\n", 1)[1].split("# Fetch our public IP", 1)[0]

class SecurityGroupTests(unittest.TestCase):
    def run_creation(self, result="sg-owned", failure="", tags_fail=False):
        script = """set -eu
SGNAME=gateway-bench-sg-test; RUN_ID=test-run; CREATED_SG=0
aws() {
  case "$2" in
    create-security-group) printf '%s\n' "$MOCK_RESULT"; [ -z "$MOCK_FAILURE" ] ;;
    create-tags) printf 'tagging %s\n' "$*" >&2; [ "$MOCK_TAGS_FAIL" = no ] ;;
    *) echo "unexpected AWS operation: $*" >&2; return 1 ;;
  esac
}
""" + BLOCK + '\nprintf "owned=%s id=%s\n" "$CREATED_SG" "$SG"\n'
        return subprocess.run(["bash", "-c", script], env={"MOCK_RESULT": result, "MOCK_FAILURE": failure, "MOCK_TAGS_FAIL": "yes" if tags_fail else "no"}, capture_output=True, text=True)

    def test_created_group_is_owned_and_tagged_for_this_run(self):
        r = self.run_creation()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("owned=1 id=sg-owned", r.stdout)
        self.assertIn("Key=run,Value=test-run", r.stderr)
        self.assertIn("Key=managed-by,Value=gateway-bench", r.stderr)

    def test_creation_and_tag_failures_abort_before_launch(self):
        for kw in ({"failure": "error"}, {"result": "None"}, {"tags_fail": True}):
            with self.subTest(kw=kw):
                self.assertNotEqual(self.run_creation(**kw).returncode, 0)

if __name__ == "__main__":
    unittest.main()
