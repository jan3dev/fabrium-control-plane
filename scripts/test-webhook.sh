#!/bin/bash
# Test the GitHub webhook endpoint with a signed payload.
# Usage: ./scripts/test-webhook.sh <CP_HOST> <WEBHOOK_SECRET> [event_type]
#
# Examples:
#   ./scripts/test-webhook.sh https://cp.fabrium.net my-secret
#   ./scripts/test-webhook.sh https://cp.fabrium.net my-secret issues

set -euo pipefail

CP_HOST="${1:-}"
WEBHOOK_SECRET="${2:-}"
EVENT_TYPE="${3:-ping}"

if [[ -z "$CP_HOST" || -z "$WEBHOOK_SECRET" ]]; then
    echo "Usage: $0 <CP_HOST> <WEBHOOK_SECRET> [event_type]"
    echo ""
    echo "Event types: ping, issues"
    exit 1
fi

DELIVERY_ID="test-$(date +%s)-$$"

case "$EVENT_TYPE" in
    ping)
        PAYLOAD='{"zen":"Responsive is better than fast.","hook_id":12345}'
        ;;
    issues)
        PAYLOAD='{
            "action": "labeled",
            "issue": {
                "number": 1,
                "title": "Test Issue",
                "body": "## Goal\n\nTest.\n\n## Acceptance criteria\n\n- Works",
                "labels": [{"name": "factory:ready"}]
            },
            "label": {"name": "factory:ready"},
            "repository": {"full_name": "test/sandbox"},
            "sender": {"type": "User"}
        }'
        ;;
    *)
        echo "Unknown event type: $EVENT_TYPE"
        echo "Supported: ping, issues"
        exit 1
        ;;
esac

SIGNATURE="sha256=$(echo -n "$PAYLOAD" | openssl dgst -sha256 -hmac "$WEBHOOK_SECRET" | awk '{print $NF}')"

echo "Sending $EVENT_TYPE event to $CP_HOST/v1/github/webhook"
echo "Delivery ID: $DELIVERY_ID"
echo "Signature: $SIGNATURE"
echo ""

HTTP_CODE=$(curl -s -w "%{http_code}" -o /tmp/webhook-response.json \
    -X POST "$CP_HOST/v1/github/webhook" \
    -H "Content-Type: application/json" \
    -H "X-GitHub-Event: $EVENT_TYPE" \
    -H "X-GitHub-Delivery: $DELIVERY_ID" \
    -H "X-Hub-Signature-256: $SIGNATURE" \
    -d "$PAYLOAD")

echo "HTTP Status: $HTTP_CODE"
echo "Response:"
cat /tmp/webhook-response.json
echo ""

if [[ "$HTTP_CODE" == "200" ]]; then
    echo "✓ Webhook accepted"
elif [[ "$HTTP_CODE" == "403" ]]; then
    echo "✗ Signature verification failed - check WEBHOOK_SECRET"
elif [[ "$HTTP_CODE" == "404" ]]; then
    echo "✗ Factory not configured on control plane"
else
    echo "✗ Unexpected response"
fi
