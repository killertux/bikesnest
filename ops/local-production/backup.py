#!/usr/bin/env python3
"""Create a consistent PostgreSQL dump and upload it to the private backup bucket."""
import datetime
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]
BUCKET = "gs://bikesnest-backups-934690449432"


def main():
    name = "bikesnest-" + datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ") + ".dump"
    compose = ["docker", "compose", "--env-file", str(ROOT / ".env"),
               "-f", str(ROOT / "ops/local-production/compose.yml")]
    with tempfile.TemporaryDirectory(prefix="bikesnest-backup-") as directory:
        dump = Path(directory) / name
        with dump.open("wb") as output:
            dump.chmod(0o600)
            subprocess.run(compose + ["exec", "-T", "db", "pg_dump", "-U", "bikesnest_admin",
                                     "-d", "bikesnest", "-Fc"], stdout=output, check=True)
        if dump.stat().st_size == 0:
            raise RuntimeError("Refusing to upload an empty database backup")
        subprocess.run(["gcloud", "storage", "cp", str(dump), BUCKET + "/" + name,
                        "--project=bikesnest"], check=True)
    print("Database backup uploaded: " + name)


if __name__ == "__main__":
    main()
