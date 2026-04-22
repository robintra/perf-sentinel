# Guide de déploiement Helm

Ce guide décrit le déploiement de perf-sentinel sur Kubernetes via le chart Helm packagé sous [`charts/perf-sentinel/`](../../charts/perf-sentinel/). Le chart déploie le daemon (`perf-sentinel watch`) derrière un Service `ClusterIP` qui expose OTLP gRPC (4317) et OTLP HTTP plus `/metrics` plus `/api/*` (4318).

Pour une alternative sans Helm, voir les manifests bruts dans [`docs/FR/INTEGRATION-FR.md`](./INTEGRATION-FR.md#déploiement-kubernetes).

## TL;DR

```bash
git clone https://github.com/robintra/perf-sentinel.git
cd perf-sentinel
helm install perf-sentinel ./charts/perf-sentinel \
  --namespace observability --create-namespace
kubectl --namespace observability get pods -l app.kubernetes.io/name=perf-sentinel
```

Une fois le pod prêt, pointez votre OpenTelemetry Collector vers `perf-sentinel.observability.svc.cluster.local:4317` (gRPC) ou `:4318` (HTTP). Un exemple complet qui compose perf-sentinel avec le chart upstream OTel Collector vit sous [`examples/helm/`](../../examples/helm/).

## Topologie

Le chart est sentinel-only par construction. Les utilisateurs composent perf-sentinel avec le chart upstream [open-telemetry/opentelemetry-collector](https://github.com/open-telemetry/opentelemetry-helm-charts) plutôt que d'embarquer un collector qui dériverait des releases upstream.

```mermaid
flowchart LR
    subgraph apps [Namespaces applicatifs]
        A[api-gateway]
        B[order-svc]
        C[payment-svc]
        D[chat-svc]
    end
    subgraph obs [namespace observability]
        OC[OTel Collector<br/>open-telemetry/opentelemetry-collector]
        PS[perf-sentinel<br/>ce chart]
    end
    subgraph mon [namespace monitoring]
        T[Tempo]
    end
    A -->|OTLP ou Zipkin| OC
    B -->|OTLP ou Zipkin| OC
    C -->|OTLP ou Zipkin| OC
    D -->|OTLP ou Zipkin| OC
    OC -->|OTLP gRPC 4317| T
    OC -->|OTLP gRPC 4317| PS
```

## Installation depuis un checkout local

Le Sprint B publiera le chart sur un registre OCI (`oci://ghcr.io/robintra/charts/perf-sentinel`) via un workflow GitHub Actions. En attendant, l'installation se fait depuis un clone local :

```bash
git clone https://github.com/robintra/perf-sentinel.git
cd perf-sentinel

# Inspectez ou surchargez les valeurs par défaut avant install.
helm show values ./charts/perf-sentinel > my-values.yaml

helm install perf-sentinel ./charts/perf-sentinel \
  --namespace observability --create-namespace \
  -f my-values.yaml
```

Le `version` du chart et l'`appVersion` sont découplés : `version` désigne la release du chart, `appVersion` désigne le tag de l'image daemon livrée avec. En production, pinnez explicitement le tag via `image.tag`.

## Modes de workload

Le chart accepte trois valeurs pour `workload.kind`. Choisissez-en une par installation.

### `Deployment` (par défaut)

Un daemon unique derrière un Service `ClusterIP`. C'est la topologie recommandée. perf-sentinel est stateful par trace (la `TraceWindow` vit en mémoire), donc exécuter un seul daemon et scaler verticalement est le bon premier mouvement. La [topologie shardée](../../examples/docker-compose-sharded.yml) est disponible pour des déploiements multi-daemon. Elle repose sur un consistent hashing par `trace_id` dans le `loadbalancingexporter` du Collector OTel afin que toutes les spans d'une trace atterrissent sur la même instance daemon.

```yaml
workload:
  kind: Deployment
  replicas: 1
```

### `DaemonSet`

Rare. Utile uniquement si vous avez une exigence dure d'avoir un daemon sur chaque noeud (par exemple pour remplacer un forwarder de traces node-local existant). Un DaemonSet répartit les traces sur plusieurs noeuds, ce qui casse la détection N+1 sauf si un collector en amont garantit que toutes les spans d'une trace rejoignent le même daemon. La plupart des utilisateurs n'ont pas besoin de ce mode.

```yaml
workload:
  kind: DaemonSet
```

### `StatefulSet`

Réservé à une future persistance sur disque. Le chart provisionne le volumeClaimTemplate de bout en bout pour que le toggle fonctionne dès aujourd'hui, mais aucune feature daemon n'écrit actuellement sous `/var/lib/perf-sentinel`. N'utilisez ce mode que si vous prototypez une extension de persistance.

```yaml
workload:
  kind: StatefulSet
  replicas: 1
  statefulset:
    persistence:
      enabled: true
      size: 5Gi
      storageClass: gp3
```

## Surface de configuration

Le chart monte une unique ConfigMap sur `/etc/perf-sentinel/.perf-sentinel.toml`. Éditez le contenu via `values.yaml` :

```yaml
config:
  toml: |
    [thresholds]
    n_plus_one_sql_critical_max = 0
    io_waste_ratio_max = 0.25

    [green]
    enabled = true
    default_region = "eu-west-3"

    [daemon]
    listen_address = "0.0.0.0"
    environment = "production"
```

Référence complète des champs : [`docs/FR/CONFIGURATION-FR.md`](./CONFIGURATION-FR.md).

### Secrets

Le fichier TOML ne doit jamais contenir de secrets (le daemon rejette les champs credentiels au chargement de la config). Injectez les valeurs sensibles via des variables d'environnement alimentées par un Secret :

```bash
kubectl -n observability create secret generic perf-sentinel-secrets \
  --from-literal=PERF_SENTINEL_EMAPS_TOKEN=sk-your-token
```

```yaml
extraEnvFrom:
  - secretRef:
      name: perf-sentinel-secrets
```

perf-sentinel ne lit pas directement les variables d'environnement pour sa configuration. Le pattern est donc : le Secret entre dans l'environnement du pod, et le fichier de configuration référence les variables via `env:VAR_NAME` pour les quelques champs qui supportent cette forme (par exemple `api_key` pour Electricity Maps). Voir la section "Environment variables" de `docs/FR/CONFIGURATION-FR.md`.

### Fichiers de calibration et certificats TLS

Les deux passent par `extraVolumes` plus `extraVolumeMounts` :

```yaml
extraVolumes:
  - name: tls
    secret:
      secretName: perf-sentinel-tls
      defaultMode: 0400
extraVolumeMounts:
  - name: tls
    mountPath: /etc/tls
    readOnly: true

config:
  toml: |
    [daemon]
    tls_cert_path = "/etc/tls/tls.crt"
    tls_key_path = "/etc/tls/tls.key"
```

## Observabilité

### ServiceMonitor Prometheus

Quand le Prometheus Operator est installé, basculez `serviceMonitor.enabled` à `true` pour scraper `/metrics` sur le port 4318 :

```yaml
serviceMonitor:
  enabled: true
  interval: 15s
  scrapeTimeout: 10s
  labels:
    # Adaptez au sélecteur de votre ressource Prometheus.
    release: prometheus
```

### Exemplars

perf-sentinel émet des exemplars Prometheus sur `perf_sentinel_findings_total`, `perf_sentinel_io_waste_ratio` et `perf_sentinel_slow_duration_seconds`. Activez le stockage des exemplars côté Prometheus :

```yaml
prometheus:
  prometheusSpec:
    enableFeatures:
      - exemplar-storage
```

Puis configurez Grafana pour cliquer de la métrique vers la trace :

```yaml
datasources:
  - name: Prometheus
    type: prometheus
    jsonData:
      exemplarTraceIdDestinations:
        - name: trace_id
          datasourceUid: tempo
```

### Sans le Prometheus Operator

Si vous utilisez un Prometheus vanilla sans operator, ajoutez une entrée de scrape statique :

```yaml
scrape_configs:
  - job_name: perf-sentinel
    kubernetes_sd_configs:
      - role: service
        namespaces:
          names: [observability]
    relabel_configs:
      - source_labels: [__meta_kubernetes_service_label_app_kubernetes_io_name]
        regex: perf-sentinel
        action: keep
      - source_labels: [__meta_kubernetes_endpoint_port_name]
        regex: otlp-http
        action: keep
```

## Mise à jour

```bash
helm upgrade perf-sentinel ./charts/perf-sentinel \
  --namespace observability \
  -f my-values.yaml
```

Le daemon ne recharge pas sa config à chaud, donc les modifications de `config.toml` exigent un redémarrage du pod. Le chart gère cela automatiquement : une annotation `checksum/config` sur le pod template calcule un hash de la ConfigMap rendue, donc chaque édition de config bump l'annotation et déclenche un rolling restart. Aucun `kubectl rollout restart` manuel n'est nécessaire.

Lors d'un bump du chart vers un nouveau `appVersion`, pinnez `image.tag` explicitement et relisez `CHANGELOG.md` pour repérer les breaking changes de config. Le chart ne valide pas encore que la version du daemon corresponde à la version du chart, cette responsabilité incombe à l'opérateur.

## Désinstallation

```bash
helm uninstall perf-sentinel --namespace observability
```

Cela supprime le Deployment, le Service, la ConfigMap, le ServiceAccount et (quand ils sont créés) le ServiceMonitor et la NetworkPolicy. Le mode StatefulSet avec persistance conserve les PersistentVolumeClaims sous-jacents par défaut, conformément à la sémantique Kubernetes. Supprimez-les explicitement si vous voulez nettoyer l'état :

```bash
kubectl --namespace observability delete pvc \
  -l app.kubernetes.io/instance=perf-sentinel
```

## Exemple bout en bout

[`examples/helm/`](../../examples/helm/) fournit deux fichiers de valeurs qui composent le chart perf-sentinel avec le chart upstream OTel Collector dans une topologie fan-out Zipkin + OTLP vers Tempo et perf-sentinel. Suivez le README de ce répertoire pour la recette complète d'installation et de vérification.
