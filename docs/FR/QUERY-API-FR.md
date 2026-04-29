# API de requêtage du daemon

Le daemon perf-sentinel expose une API HTTP de requêtage qui permet à des
systèmes externes de récupérer les findings, les explications de traces,
les corrélations cross-trace et la liveness du daemon. Utilisez-la pour
alimenter des alertes Prometheus, des dashboards Grafana, des runbooks
on-call ou des scripts de gate CI personnalisés sans parser les logs
NDJSON.

L'API a été livrée en v0.4.0. Cette page la documente comme
surface produit de premier plan, avec un contrat de stabilité.

## Sommaire

- [Vue d'ensemble des endpoints](#vue-densemble-des-endpoints) : description en une ligne par endpoint.
- [Endpoints](#endpoints) : référence complète par endpoint avec requête, réponse et exemples concrets.
- [Réponses d'erreur](#réponses-derreur) : codes de statut et formes du body.
- [Cas d'usage](#cas-dusage) : alerting Prometheus, panneaux Grafana custom, runbooks SRE.
- [Contrat de stabilité](#contrat-de-stabilité) : garanties de stabilité v0.4.1+.
- [Voir aussi](#voir-aussi) : références croisées vers les docs reliées.

## Vue d'ensemble des endpoints

| Méthode | Chemin                     | Rôle                                                                           |
|---------|----------------------------|--------------------------------------------------------------------------------|
| GET     | `/api/status`              | Liveness du daemon, version, uptime, compteurs en cours                        |
| GET     | `/api/findings`            | Findings récents depuis le ring buffer, avec filtres service, type et severity |
| GET     | `/api/findings/{trace_id}` | Tous les findings d'une trace                                                  |
| GET     | `/api/explain/{trace_id}`  | Arbre de spans d'une trace encore en mémoire daemon, findings annotés en ligne |
| GET     | `/api/correlations`        | Corrélations temporelles cross-trace actives                                   |

Tous les endpoints retournent du `application/json`. Pas
d'authentification. Le daemon écoute sur `127.0.0.1` par défaut (voir
`[daemon] listen_address` dans `docs/FR/CONFIGURATION-FR.md`), donc l'API
n'est joignable que depuis l'hôte qui exécute le daemon, sauf si vous
élargissez explicitement l'adresse de bind.

### Notes de déploiement

- L'API de requêtage partage le même port HTTP que l'ingestion OTLP HTTP
  (`[daemon] listen_port_http`, défaut `4318`), l'endpoint Prometheus
  `/metrics` et la sonde de liveness `GET /health`. Un seul port,
  quatre surfaces.
- L'API de requêtage peut être désactivée au démarrage avec
  `[daemon] api_enabled = false`. Utile quand le daemon tourne dans un
  hôte multi-tenant hostile et que vous ne voulez que l'ingestion OTLP.
  Dans ce mode, `/metrics` et `/health` restent exposés : ce sont des
  surfaces d'infrastructure, pas partie de l'API de requêtage.
- Pour les sondes Kubernetes ou load-balancer, préférer `GET /health` à
  `GET /api/status` : `/health` est toujours actif, ne prend aucun lock
  et reste réactif sous toute charge d'ingestion.
- La taille du ring buffer des findings est bornée par
  `[daemon] max_retained_findings` (défaut `10000`). Les findings plus
  anciens sont évincés en FIFO.

## Endpoints

### GET /api/status

Retourne un objet de liveness compact. Utilisez-le comme readiness probe
ou comme moyen le moins coûteux de vérifier que le daemon est up.

**Paramètres de requête :** aucun.

**Forme de la réponse :**

| Champ             | Type   | Description                                                  |
|-------------------|--------|--------------------------------------------------------------|
| `version`         | string | Version du binaire daemon (version du package Cargo)         |
| `uptime_seconds`  | number | Secondes depuis le démarrage du processus daemon             |
| `active_traces`   | number | Traces actuellement présentes dans la fenêtre de corrélation |
| `stored_findings` | number | Findings actuellement retenus dans le ring buffer            |

**Exemple :**

```bash
curl -sS http://127.0.0.1:4318/api/status
```

```json
{
  "version": "0.4.0",
  "uptime_seconds": 48,
  "active_traces": 0,
  "stored_findings": 5
}
```

### GET /api/findings

Retourne un tableau JSON des findings récents, du plus récent au plus
ancien. Chaque élément encapsule le finding lui-même plus un timestamp
d'ingestion côté daemon.

**Paramètres de requête :**

| Nom        | Type    | Défaut | Description                                                                                                             |
|------------|---------|--------|-------------------------------------------------------------------------------------------------------------------------|
| `service`  | string  | aucun  | Match exact sur le champ `finding.service`                                                                              |
| `type`     | string  | aucun  | Match exact sur `finding.type` en snake_case (ex. `n_plus_one_sql`, `redundant_sql`)                                    |
| `severity` | string  | aucun  | Match exact sur `finding.severity` en snake_case (`critical`, `warning`, `info`)                                        |
| `limit`    | integer | `100`  | Nombre maximum d'entrées retournées, capé côté serveur à `1000` (les valeurs supérieures sont silencieusement ramenées) |

Les paramètres inconnus sont ignorés. Les valeurs malformées (ex.
`limit=abc`) retournent un HTTP 400 avec un corps d'erreur généré par
axum.

**Forme de la réponse :** tableau de `StoredFinding`. Chaque
`StoredFinding` contient :

- `finding` : le finding détecté. Voir
  [le schéma `Finding`](#schéma-finding) ci-dessous.
- `stored_at_ms` : timestamp Unix entier en millisecondes, enregistré au
  moment où le daemon a inséré ce finding dans le ring buffer.

**Exemple :**

```bash
curl -sS "http://127.0.0.1:4318/api/findings?severity=warning&limit=2"
```

```json
[
  {
    "finding": {
      "type": "n_plus_one_sql",
      "severity": "warning",
      "trace_id": "trace-n1-sql",
      "service": "order-svc",
      "source_endpoint": "POST /api/orders/42/submit",
      "pattern": {
        "template": "SELECT * FROM order_item WHERE order_id = ?",
        "occurrences": 6,
        "window_ms": 250,
        "distinct_params": 6
      },
      "suggestion": "Use WHERE ... IN (?) to batch 6 queries into one",
      "first_timestamp": "2025-07-10T14:32:01.000Z",
      "last_timestamp": "2025-07-10T14:32:01.250Z",
      "green_impact": {
        "estimated_extra_io_ops": 5,
        "io_intensity_score": 6.0,
        "io_intensity_band": "high"
      },
      "confidence": "daemon_staging"
    },
    "stored_at_ms": 1776350162450
  },
  {
    "finding": {
      "type": "n_plus_one_http",
      "severity": "warning",
      "trace_id": "trace-n1-http",
      "service": "order-svc",
      "source_endpoint": "POST /api/orders/42/submit",
      "pattern": {
        "template": "GET /api/users/{id}",
        "occurrences": 6,
        "window_ms": 200,
        "distinct_params": 6
      },
      "suggestion": "Use batch endpoint with ?ids=... to batch 6 calls into one",
      "first_timestamp": "2025-07-10T14:32:01.000Z",
      "last_timestamp": "2025-07-10T14:32:01.200Z",
      "green_impact": {
        "estimated_extra_io_ops": 5,
        "io_intensity_score": 6.0,
        "io_intensity_band": "high"
      },
      "confidence": "daemon_staging"
    },
    "stored_at_ms": 1776350162450
  }
]
```

#### Schéma Finding

L'objet `finding` exposé par `/api/findings` et
`/api/findings/{trace_id}` est identique au JSON émis par
`perf-sentinel analyze --format json`. Champs stables à partir de
v0.4.1 :

| Champ             | Type               | Description                                                                                                                                                                  |
|-------------------|--------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `type`            | string (enum)      | `n_plus_one_sql`, `n_plus_one_http`, `redundant_sql`, `redundant_http`, `slow_sql`, `slow_http`, `excessive_fanout`, `chatty_service`, `pool_saturation`, `serialized_calls` |
| `severity`        | string (enum)      | `critical`, `warning`, `info`                                                                                                                                                |
| `trace_id`        | string             | Trace ID où le pattern a été détecté                                                                                                                                         |
| `service`         | string             | Service qui a émis l'anti-pattern                                                                                                                                            |
| `source_endpoint` | string             | Endpoint entrant normalisé qui héberge le pattern                                                                                                                            |
| `pattern`         | object             | `{ template, occurrences, window_ms, distinct_params }`                                                                                                                      |
| `suggestion`      | string             | Indication de remédiation lisible                                                                                                                                            |
| `first_timestamp` | string (ISO 8601)  | Premier span du groupe détecté                                                                                                                                               |
| `last_timestamp`  | string (ISO 8601)  | Dernier span du groupe détecté                                                                                                                                               |
| `confidence`      | string (enum)      | `ci_batch`, `daemon_staging`, `daemon_production`                                                                                                                            |
| `green_impact`    | object (optionnel) | `{ estimated_extra_io_ops, io_intensity_score, io_intensity_band }` quand le scoring green est activé                                                                        |
| `code_location`   | object (optionnel) | `{ function?, filepath?, lineno?, namespace? }` quand les attributs OTel `code.*` sont présents                                                                              |
| `suggested_fix`   | object (optionnel) | `{ pattern, framework, recommendation, reference_url? }` quand le framework peut être inféré (Java/JPA en v1)                                                                |

### GET /api/findings/{trace_id}

Retourne tous les findings dont le `trace_id` matche le segment de
chemin, sous forme de tableau JSON. Même forme d'élément que
`/api/findings`. Le cap dur de 1000 entrées s'applique (traces
pathologiques avec des centaines de clusters N+1).

**Paramètre de chemin :** `trace_id` (string, match exact). Le segment
est URL-décodé par axum avant comparaison.

**Forme de la réponse :** même `Vec<StoredFinding>` que `/api/findings`.
Un **tableau vide `[]`** est retourné quand le trace ID est inconnu
(l'endpoint ne renvoie pas 404).

**Exemple :**

```bash
curl -sS "http://127.0.0.1:4318/api/findings/trace-n1-sql"
```

```json
[
  {
    "finding": {
      "type": "n_plus_one_sql",
      "severity": "warning",
      "trace_id": "trace-n1-sql",
      "service": "order-svc",
      "source_endpoint": "POST /api/orders/42/submit",
      "pattern": {
        "template": "SELECT * FROM order_item WHERE order_id = ?",
        "occurrences": 6,
        "window_ms": 250,
        "distinct_params": 6
      },
      "suggestion": "Use WHERE ... IN (?) to batch 6 queries into one",
      "first_timestamp": "2025-07-10T14:32:01.000Z",
      "last_timestamp": "2025-07-10T14:32:01.250Z",
      "green_impact": {
        "estimated_extra_io_ops": 5,
        "io_intensity_score": 6.0,
        "io_intensity_band": "high"
      },
      "confidence": "daemon_staging"
    },
    "stored_at_ms": 1776350162450
  }
]
```

### GET /api/explain/{trace_id}

Retourne l'arbre de spans d'une trace **encore présente dans la fenêtre
de corrélation du daemon** (TTL par défaut : 30 secondes après l'arrivée
du dernier span de la trace). Utile pour debugger une trace live juste
après son émission.

**Important :** les findings sont retenus dans le ring buffer longtemps
après que la trace elle-même ait été évincée de la fenêtre. Cela veut
dire que `/api/findings/{trace_id}` continue à fonctionner pendant des
heures après que la trace a disparu, mais que `/api/explain/{trace_id}`
ne fonctionne que pendant la TTL de la fenêtre.

**Paramètre de chemin :** `trace_id` (string, match exact).

**Forme de la réponse (trace en mémoire) :** objet avec un tableau
`roots`. Chaque nœud décrit un span avec :

| Champ            | Type           | Description                                                                        |
|------------------|----------------|------------------------------------------------------------------------------------|
| `span_id`        | string         | Identifiant du span                                                                |
| `parent_span_id` | string \| null | Identifiant du span parent, `null` pour les spans racines                          |
| `service`        | string         | Service qui a émis le span                                                         |
| `operation`      | string         | Nom de l'opération (ex. `SELECT`, `GET`, `POST`)                                   |
| `template`       | string         | Requête SQL ou route HTTP normalisée                                               |
| `timestamp`      | string         | Timestamp de début ISO 8601                                                        |
| `duration_us`    | number         | Durée en microsecondes                                                             |
| `findings`       | array          | Findings rattachés à ce span, chacun `{ type, severity, suggestion, occurrences }` |
| `children`       | array          | Nœuds spans enfants, récursif                                                      |

**Forme de la réponse (trace inconnue ou évincée) :** un objet avec un
seul champ `error`.

**Exemples :**

```bash
# Trace encore en mémoire
curl -sS "http://127.0.0.1:4318/api/explain/trace-n1-sql"
```

```json
{
  "roots": [
    {
      "children": [],
      "duration_us": 800,
      "findings": [
        {
          "occurrences": 6,
          "severity": "warning",
          "suggestion": "Use WHERE ... IN (?) to batch 6 queries into one",
          "type": "n_plus_one_sql"
        }
      ],
      "operation": "SELECT",
      "parent_span_id": null,
      "service": "order-svc",
      "span_id": "span-1",
      "template": "SELECT * FROM order_item WHERE order_id = ?",
      "timestamp": "2025-07-10T14:32:01.000Z"
    }
  ]
}
```

```bash
# Trace pas en mémoire (évincée ou jamais vue)
curl -sS "http://127.0.0.1:4318/api/explain/trace-does-not-exist"
```

```json
{
  "error": "trace not found in daemon memory"
}
```

### GET /api/correlations

Retourne les corrélations temporelles cross-trace actives, triées par
confiance décroissante. Tableau vide quand
`[daemon.correlation] enabled = false` (défaut). Capé à 1000 entrées.

**Paramètres de requête :** aucun.

**Forme de la réponse :** tableau de `CrossTraceCorrelation`. Chaque
entrée contient :

| Champ                      | Type    | Description                                                                       |
|----------------------------|---------|-----------------------------------------------------------------------------------|
| `source`                   | object  | Endpoint en tête : `{ finding_type, service, template }`                          |
| `target`                   | object  | Endpoint en queue observé après `source` dans `lag_threshold_ms`                  |
| `co_occurrence_count`      | number  | Nombre de co-occurrences dans la fenêtre roulante                                 |
| `source_total_occurrences` | number  | Occurrences totales de `source` dans la fenêtre roulante                          |
| `confidence`               | number  | Ratio `co_occurrence_count / source_total_occurrences`                            |
| `median_lag_ms`            | number  | Lag médian entre `source` et `target`                                             |
| `first_seen`               | string  | Timestamp ISO 8601 de la première co-occurrence                                   |
| `last_seen`                | string  | Timestamp ISO 8601 de la co-occurrence la plus récente                            |

**Exemple :**

```bash
curl -sS "http://127.0.0.1:4318/api/correlations"
```

```json
[
  {
    "source": {
      "finding_type": "redundant_sql",
      "service": "cache-svc",
      "template": "SELECT * FROM settings WHERE key = ?"
    },
    "target": {
      "finding_type": "n_plus_one_sql",
      "service": "order-svc",
      "template": "SELECT * FROM order_item WHERE order_id = ?"
    },
    "co_occurrence_count": 2,
    "source_total_occurrences": 1,
    "confidence": 2.0,
    "median_lag_ms": 0.0,
    "first_seen": "2026-04-16T14:36:02.450Z",
    "last_seen": "2026-04-16T14:36:02.450Z"
  }
]
```

### GET /api/export/report

Snapshot de l'état interne courant du daemon sous forme de JSON `Report`, avec la même forme que `perf-sentinel analyze --format json`. Ferme la boucle entre le daemon live et le dashboard HTML `perf-sentinel report` post-mortem : le rapport HTML peut ingérer un snapshot daemon via HTTP par simple composition shell.

La section `analysis` reflète les compteurs lifetime du daemon (cumulatifs depuis le démarrage). Le champ `green_summary` est rafraîchi par l'event loop après chaque batch (régions, top offenders, ratio d'I/O évitables, chiffres CO2, scoring config), donc le snapshot porte une photo CO2 vivante. Le bandeau de chips et le tab GreenOps du dashboard HTML apparaissent naturellement sur les daemons configurés avec Electricity Maps. La quality gate n'est pas recalculée sur le chemin snapshot. Voir `docs/FR/design/05-GREENOPS-AND-CARBON-FR.md` pour le récit complet du chemin d'audit.

**Comportement cold-start.** Quand le daemon n'a encore traité aucun événement, l'endpoint retourne `503 Service Unavailable` avec le corps `{"error": "daemon has not yet processed any events"}`. Ça distingue "cold start" de "événements vus, zéro finding" (ce dernier retourne `200` avec un tableau `findings` vide, qui est un Report valide).

**Métrique Prometheus.** Chaque requête incrémente `perf_sentinel_export_report_requests_total`, les opérateurs peuvent donc dashboarder ou alerter sur la fréquence des snapshots.

Exemple :

```bash
# Matérialiser un snapshot daemon live en dashboard HTML
curl -s http://daemon.internal:4318/api/export/report \
    | perf-sentinel report --input - --output report.html
```

La sous-commande `report` auto-détecte la forme JSON : un tableau au top-level est traité comme des événements de trace (passés dans normalize + detect + score), un objet au top-level est traité comme un Report pré-calculé (pris tel quel). L'onglet Correlations du dashboard HTML s'active automatiquement quand le Report produit par le daemon porte des `correlations` non vides.

## Réponses d'erreur

| Condition                                         | Status | Corps                                                  |
|---------------------------------------------------|--------|--------------------------------------------------------|
| `trace_id` inconnu sur `/api/findings/{trace_id}` | 200    | `[]`                                                   |
| `trace_id` inconnu sur `/api/explain/{trace_id}`  | 200    | `{"error": "trace not found in daemon memory"}`        |
| Corrélations désactivées ou correlator inactif    | 200    | `[]`                                                   |
| `/api/export/report` sur daemon cold-start        | 503    | `{"error": "daemon has not yet processed any events"}` |
| Paramètre de requête malformé (ex. `limit=abc`)   | 400    | erreur en texte brut générée par axum           |
| Chemin inconnu (ex. `/api/does-not-exist`)        | 404    | corps vide                                      |
| Méthode autre que GET                             | 405    | erreur en texte brut générée par axum           |

L'API n'émet pas de 5xx en fonctionnement normal. Un crash du processus
retourne ce que la pile TCP émet (connection reset).

## Cas d'usage

### Alerting Prometheus sur les findings critiques

Faites tourner un Prometheus Blackbox exporter qui scrape
`/api/findings?severity=critical&limit=1` et alerte quand le tableau de
réponse est non-vide. Exemple de règle AlertManager utilisant un
`vector_count` calculé par une recording rule :

```yaml
groups:
  - name: perf-sentinel
    rules:
      - alert: PerfSentinelCriticalFinding
        expr: perf_sentinel_findings_total{severity="critical"} > 0
        for: 2m
        labels:
          severity: page
        annotations:
          summary: "perf-sentinel a détecté un anti-pattern de performance critique"
          description: |
            Compteur de findings critiques: {{ $value }}.
            Interrogez `/api/findings?severity=critical` sur le daemon pour les détails.
```

L'endpoint Prometheus intégré à `/metrics` expose déjà
`perf_sentinel_findings_total{type,severity}` comme compteur, donc vous
n'avez pas besoin de l'API de requêtage pour compter les alertes.
Utilisez l'API de requêtage pour récupérer le **payload** (template,
trace ID, suggestion) que le handler d'alerte inclut dans la
notification.

### Dashboard Grafana custom via le datasource JSON

Installez le plugin Grafana JSON API datasource, pointez-le vers le
daemon et construisez des tableaux par service. Exemple de requête de
panel qui retourne les 20 findings les plus récents pour `order-svc` :

```
URL :     http://perf-sentinel.internal:4318/api/findings
Méthode : GET
Params :  service=order-svc
          limit=20
Champs :  $.finding.type,
          $.finding.severity,
          $.finding.pattern.template,
          $.finding.pattern.occurrences,
          $.finding.source_endpoint,
          $.stored_at_ms
```

Couplez cela avec l'endpoint Prometheus `/metrics` déjà exposé par le
daemon pour les tendances time-series et utilisez l'API de requêtage
pour la **liste de findings concrets** sur lesquels l'utilisateur peut
cliquer.

### Runbook SRE : page sur scraper bloqué

Si votre daemon a un scraper opt-in configuré (`[green.scaphandre]`,
`[green.cloud]`, `[green.electricity_maps]`, `[pg_stat]`), une stagnation
dans `active_traces` ou la croissance de `stored_findings` est un signal
fort que l'ingestion est bloquée. Snippet bash à embarquer dans un
runbook on-call :

```bash
#!/usr/bin/env bash
set -euo pipefail

DAEMON="${DAEMON:-http://127.0.0.1:4318}"
response=$(curl -sSf --max-time 3 "${DAEMON}/api/status")
uptime=$(echo "$response" | jq -r '.uptime_seconds')
traces=$(echo "$response" | jq -r '.active_traces')
findings=$(echo "$response" | jq -r '.stored_findings')

if [ "$uptime" -gt 300 ] && [ "$traces" -eq 0 ] && [ "$findings" -eq 0 ]; then
  echo "Le daemon perf-sentinel est inactif depuis ${uptime}s sans traces ni findings"
  echo "Vérifier le chemin d'ingestion: endpoint OTLP, config collector, env vars Java agent"
  exit 1
fi
```

Branchez ceci à PagerDuty ou OpsGenie via l'outil d'escalation on-call de
votre choix.

## Contrat de stabilité

L'API de requêtage porte une promesse de stabilité à partir de v0.4.1.

**Ce qui est stable :**

- Tous les chemins listés dans
  [Vue d'ensemble des endpoints](#vue-densemble-des-endpoints).
- Tous les champs listés dans les sections d'endpoints ci-dessus. Les
  noms et formes de champs ne seront pas renommés, retirés ou retypés
  dans une release mineure.
- Les valeurs d'enum (`finding.type`, `finding.severity`,
  `finding.confidence`, `io_intensity_band`, etc.) : les variantes
  existantes restent. De nouvelles variantes peuvent être ajoutées dans
  les releases mineures. Les clients doivent tolérer les valeurs d'enum
  inconnues sans crasher.
- Le comportement des cinq réponses d'erreur dans
  [Réponses d'erreur](#réponses-derreur).

**Ce qui peut changer dans une release mineure :**

- De nouveaux champs optionnels peuvent être ajoutés à n'importe quel
  objet JSON.
- De nouvelles variantes d'enum peuvent être ajoutées.
- De nouveaux endpoints sous `/api/...` peuvent être introduits.
- Les valeurs par défaut (ex. `limit=100`) peuvent être ajustées si le
  profilage montre un meilleur défaut, mais le cap dur (`1000`) ne se
  réduira pas.

**Ce qui requiert une release majeure :**

- Retirer ou renommer un champ.
- Retyper un champ (ex. transformer un number en string).
- Réduire le cap dur sur `/api/findings?limit=`.
- Changer la surface d'authentification (le contrat actuel est non
  authentifié, loopback-only par défaut).

**Guide pour les clients :**

- Toujours tolérer les champs inconnus dans les objets JSON.
- Ne jamais parser les variantes d'enum de manière exhaustive sans
  branche fallback.
- Pinner la version du daemon dans vos manifestes CI/CD et lire le
  `CHANGELOG.md` avant de monter de version.

## Voir aussi

- [`docs/FR/INTEGRATION-FR.md`](./INTEGRATION-FR.md) pour la topologie
  de déploiement globale.
- [`docs/FR/CONFIGURATION-FR.md`](./CONFIGURATION-FR.md) pour les
  réglages `[daemon]` et `[daemon.correlation]`.
- [`docs/FR/design/06-INGESTION-AND-DAEMON-FR.md`](./design/06-INGESTION-AND-DAEMON-FR.md)
  pour le design interne du daemon.
