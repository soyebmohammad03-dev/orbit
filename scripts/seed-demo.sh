#!/usr/bin/env bash
# Seeds a running orbit-api with a small demonstration dataset, through the
# public HTTP API only. Usage: scripts/seed-demo.sh [API_BASE_URL]
set -euo pipefail
API="${1:-http://127.0.0.1:7878/api/v1}"

post() {
  local path="$1" body="$2"
  local status
  status=$(curl -s -o /dev/null -w '%{http_code}' -X POST "$API$path" -H 'content-type: application/json' -d "$body")
  case "$status" in
    2*) ;;
    409) echo "  (already exists: $path)" ;;
    *) echo "POST $path failed with HTTP $status" >&2; exit 1 ;;
  esac
}

curl -sf "$API/health" >/dev/null || { echo "orbit-api is not reachable at $API" >&2; exit 1; }

echo "orders: merge-patch events"
post /streams '{"stream_id":"order-1042","metadata":{"tenant":"acme","channel":"web"}}'
post /streams/order-1042/events '{"event_type":"order.created","payload":{"status":"new","customer":"c-17","items":{"SKU-A":2}}}'
post /streams/order-1042/events '{"event_type":"order.item_added","payload":{"items":{"SKU-B":1}}}'
post /streams/order-1042/events '{"event_type":"order.priced","payload":{"total_cents":7450,"currency":"EUR"}}'
post /streams/order-1042/events '{"event_type":"order.paid","payload":{"status":"paid","payment":{"method":"card","ref":"pay_88"}}}'
post /streams/order-1042/events '{"event_type":"order.item_removed","payload":{"items":{"SKU-A":null},"total_cents":2450}}'
post /streams/order-1042/events '{"event_type":"order.shipped","payload":{"status":"shipped","carrier":"dhl"}}'
post /streams/order-1042/snapshots '{"at":4}'

echo "inventory: domain projection with rules"
post /streams '{"stream_id":"warehouse-north","metadata":{"region":"eu-north"}}'
post /streams/warehouse-north/events '{"event_type":"inventory.warehouse_named","payload":{"name":"North"}}'
post /streams/warehouse-north/events '{"event_type":"inventory.item_added","payload":{"sku":"A-1","name":"Bolt","quantity":10}}'
post /streams/warehouse-north/events '{"event_type":"inventory.item_added","payload":{"sku":"B-2","name":"Nut","quantity":4}}'
post /streams/warehouse-north/events '{"event_type":"inventory.reserved","payload":{"sku":"A-1","quantity":3}}'
post /streams/warehouse-north/events '{"event_type":"inventory.shipped","payload":{"sku":"A-1","quantity":2}}'
post /streams/warehouse-north/events '{"event_type":"inventory.discontinued","payload":{"sku":"B-2"}}'
post /streams/warehouse-north/snapshots '{"projection":"inventory","at":3}'

echo "counter: replay runs"
post /streams '{"stream_id":"counter-1"}'
for by in 5 3 8 1; do post /streams/counter-1/events "{\"event_type\":\"counter.incremented\",\"payload\":{\"by\":$by}}"; done
post /streams/counter-1/events '{"event_type":"counter.decremented","payload":{"by":4}}'
post /streams/counter-1/experiments '{"projection":"counter","snapshot_policy":"prefer"}'

echo "done: open the Observatory and select a stream."
