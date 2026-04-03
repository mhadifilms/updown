#!/bin/bash
# =============================================================================
# updown production deployment script
# =============================================================================
# Usage:
#   1. Point your domain's DNS A record to this server's IP
#   2. Run: UPDOWN_DOMAIN=transfer.yourdomain.com bash deploy.sh
#
# This will:
#   - Install Docker if not present
#   - Build the updown container
#   - Start Caddy (auto-HTTPS) + updown
#   - Open firewall ports (80, 443, 9000/udp)
#   - Print the admin API key
# =============================================================================

set -e

DOMAIN="${UPDOWN_DOMAIN:-localhost}"

echo "=== updown deployment ==="
echo "  Domain: $DOMAIN"
echo ""

# Check Docker
if ! command -v docker &> /dev/null; then
    echo "[1/5] Installing Docker..."
    curl -fsSL https://get.docker.com | sh
    sudo systemctl enable --now docker
else
    echo "[1/5] Docker already installed"
fi

# Check docker compose
if ! docker compose version &> /dev/null; then
    echo "ERROR: docker compose not available. Install Docker Compose v2."
    exit 1
fi

# Open firewall ports
echo "[2/5] Configuring firewall..."
if command -v ufw &> /dev/null; then
    sudo ufw allow 80/tcp
    sudo ufw allow 443/tcp
    sudo ufw allow 9000/udp
    echo "  UFW: ports 80, 443, 9000/udp opened"
elif command -v firewall-cmd &> /dev/null; then
    sudo firewall-cmd --permanent --add-port=80/tcp
    sudo firewall-cmd --permanent --add-port=443/tcp
    sudo firewall-cmd --permanent --add-port=9000/udp
    sudo firewall-cmd --reload
    echo "  firewalld: ports 80, 443, 9000/udp opened"
else
    echo "  No firewall manager detected — make sure ports 80, 443, 9000/udp are open"
fi

# Build and start
echo "[3/5] Building containers..."
export UPDOWN_DOMAIN="$DOMAIN"
docker compose build

echo "[4/5] Starting services..."
docker compose up -d

# Wait for startup
echo "[5/5] Waiting for startup..."
sleep 5

# Get admin API key from logs
echo ""
echo "=== Deployment complete ==="
echo ""
echo "  Web portal: https://$DOMAIN"
echo "  API:        https://$DOMAIN/api/health"
echo "  UDP data:   $DOMAIN:9000"
echo ""
echo "  Admin API key:"
docker compose logs updown 2>&1 | grep "api_key=" | tail -1 | sed 's/.*api_key=/  /'
echo ""
echo "  Use this key to login at https://$DOMAIN/login"
echo ""
echo "  To view logs: docker compose logs -f"
echo "  To stop:      docker compose down"
echo "  To update:    git pull && docker compose build && docker compose up -d"
