#!/bin/sh

# Shared demo-name resolution for the local Docker Compose helpers.
resolve_demo_company() {
    case "$1" in
        fund | vc | venture-capital) echo "venture_capital" ;;
        marketing | agency) echo "marketing_agency" ;;
        software | saas | dev) echo "software_company" ;;
        product | product-team) echo "product_team" ;;
        studio | venture-studio) echo "venture_studio" ;;
        accelerator) echo "startup_accelerator" ;;
        law | legal) echo "law_firm" ;;
        accounting | finance) echo "accounting_firm" ;;
        support) echo "customer_support" ;;
        signals | opportunity) echo "signals_opportunity_studio" ;;
        *) echo "$1" ;;
    esac
}

demo_project_name() {
    printf 'opencompany-%s\n' "$(printf '%s' "$1" | tr '_' '-')"
}

# The development Compose overlay gives its build caches fixed external names
# so every demo project can reuse them. Compose never creates external volumes,
# so each entry point must ensure they exist before its first Compose command.
ensure_demo_cache_volumes() {
    for cache_volume in \
        opencompany-cargo-registry \
        opencompany-cargo-git \
        opencompany-cargo-target \
        opencompany-frontend-node-modules; do
        docker volume inspect "$cache_volume" >/dev/null 2>&1 \
            || docker volume create "$cache_volume" >/dev/null
    done
}
