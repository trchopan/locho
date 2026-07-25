#!/bin/sh
set -eu
while [ ! -s /run/locho/http-attach ]; do sleep 0.2; done
set -- $(cat /run/locho/http-attach)
exec locho attach "$1" "$2" "$3" --direct-address 172.30.0.10:4567 --listen 0.0.0.0:8765
