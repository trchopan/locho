#!/bin/sh
set -eu
while [ ! -s /run/locho/tcp-attach ]; do sleep 0.2; done
set -- $(cat /run/locho/tcp-attach)
exec locho attach "$1" "$2" "$3" --tcp --direct-address 172.30.0.10:4567 --listen 0.0.0.0:9876
