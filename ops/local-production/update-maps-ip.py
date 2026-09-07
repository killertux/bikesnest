#!/usr/bin/env python3
"""Keep the server Maps key restricted to this machine's current public IPs."""
import ipaddress
import json
import subprocess
import urllib.request


def main():
    with urllib.request.urlopen("https://api4.ipify.org", timeout=20) as response:
        ipv4 = str(ipaddress.IPv4Address(response.read().decode().strip()))
    addresses = [ipv4]
    ipv6 = subprocess.run(
        ["curl", "-6", "--fail", "--silent", "--max-time", "10", "https://api6.ipify.org"],
        capture_output=True, text=True,
    )
    if ipv6.returncode == 0:
        addresses.append(str(ipaddress.IPv6Address(ipv6.stdout.strip())))
    key = json.loads(subprocess.check_output([
        "gcloud", "services", "api-keys", "describe", "bikesnest-maps-server",
        "--project=bikesnest", "--format=json(restrictions)",
    ]))
    current = key.get("restrictions", {}).get("serverKeyRestrictions", {}).get("allowedIps", [])
    if set(current) == set(addresses):
        print("Maps server IP restrictions are current.")
        return
    subprocess.run([
        "gcloud", "services", "api-keys", "update", "bikesnest-maps-server",
        "--project=bikesnest", "--allowed-ips=" + ",".join(addresses),
        "--format=none",
    ], check=True)
    print("Updated Maps server IP restrictions; API restrictions preserved.")


if __name__ == "__main__":
    main()
