# Runbook d'incident

Guide opérationnel pour perf-sentinel en production. Chaque section est autonome : partez du symptôme qui correspond au vôtre, déroulez la liste des **premiers contrôles**, puis escaladez.

Si vous configurez perf-sentinel pour la première fois, consultez [INTEGRATION-FR.md](INTEGRATION-FR.md). Pour la référence de l'API HTTP, voir [QUERY-API-FR.md](QUERY-API-FR.md). Pour les options de configuration, voir [CONFIGURATION-FR.md](CONFIGURATION-FR.md). Pour la liste de ce que le daemon ne garantit *pas*, voir [LIMITATIONS-FR.md](LIMITATIONS-FR.md).

## Sommaire

- [Aide-mémoire diagnostic](#aide-mémoire-diagnostic) : commandes à lancer en premier
- [Analyser une trace plus ancienne que la fenêtre live](#analyser-une-trace-plus-ancienne-que-la-fenêtre-live) : workflow post-mortem
- [Daemon en cours mais inaccessible depuis les clients](#daemon-en-cours-mais-inaccessible-depuis-les-clients)
- [Aucune trace ingérée](#aucune-trace-ingérée)
- [Chute soudaine du volume d'ingestion](#chute-soudaine-du-volume-dingestion)
- [Spike de findings critiques](#spike-de-findings-critiques)
- [Pression mémoire ou OOM du daemon](#pression-mémoire-ou-oom-du-daemon)
- [Quality gate CI en échec inattendu](#quality-gate-ci-en-échec-inattendu)
- [`perf-sentinel tempo` renvoie 404 ou timeout](#perf-sentinel-tempo-renvoie-404-ou-timeout)
- [Exemplars absents dans Grafana](#exemplars-absents-dans-grafana)
- [Scraper d'énergie bloqué](#scraper-dénergie-bloqué)
- [`/api/correlations` renvoie vide](#apicorrelations-renvoie-vide)
- [`/api/export/report` retourne 503 ou un rapport vide](#apiexportreport-retourne-503-ou-un-rapport-vide)
- [Crash ou redémarrage du daemon](#crash-ou-redémarrage-du-daemon)
- [Appliquer un changement de config](#appliquer-un-changement-de-config)

---

## Aide-mémoire diagnostic

À lancer en premier quel que soit le symptôme. Ça donne la photo en 10 secondes de l'état du daemon.

```bash
# Le daemon est-il vivant ? HTTP 200 avec un corps de métriques = oui.
curl -sf http://perf-sentinel:4318/metrics | head -n 20

# Résumé de statut : uptime, traces actives, findings stockés, version
curl -s http://perf-sentinel:4318/api/status | jq .

# Santé d'ingestion d'un coup d'œil
curl -s http://perf-sentinel:4318/metrics \
  | grep -E '^perf_sentinel_(events|traces|active)_'

# Findings critiques récents
curl -s 'http://perf-sentinel:4318/api/findings?severity=critical&limit=20' \
  | jq '.[].finding | {finding_type, service, trace_id}'
```

Logs du daemon avec verbosité ciblée (le daemon utilise la variable d'env `RUST_LOG` standard) :

```bash
RUST_LOG=sentinel_core::daemon=info     # cycle de vie, bind, shutdown
RUST_LOG=sentinel_core::ingest=debug    # chemin de réception OTLP, events droppés
RUST_LOG=sentinel_core::detect=debug    # pipeline de détection
RUST_LOG=sentinel_core::score=debug     # scoring green, scrapers d'énergie
```

Pour les probes Kubernetes, utilisez l'endpoint dédié `GET /health` (toujours exposé, indépendamment de `[daemon] api_enabled`), qui retourne `200 OK` avec `{"status":"ok","version":"..."}`. Plus léger que `/metrics` et garanti sans lock interne. Il n'y a pas d'endpoint `/ready` séparé : le daemon accepte l'ingestion dès le premier tick, donc liveness et readiness se confondent.

---

## Analyser une trace plus ancienne que la fenêtre live

**Pourquoi cette section existe.** Le daemon garde les traces en mémoire pendant **30 secondes** (`trace_ttl_ms`, défaut). Une fois évincée :

- `GET /api/explain/{trace_id}` renvoie `{"error": "trace not found in daemon memory"}`
- `GET /api/findings/{trace_id}` renvoie toujours les findings (conservés dans le ring buffer jusqu'à `max_retained_findings = 10000`), mais **les spans eux-mêmes ont disparu**, aucun explain tree ne peut être reconstruit depuis le daemon seul.

Pour tout ce qui est plus ancien, la source de vérité est votre backend de traces (typiquement Grafana Tempo).

**Workflow en quatre étapes.**

```
 1. Alerte           →  Panel Grafana, spike sur perf_sentinel_findings_total
 2. Clic exemplar    →  Grafana ouvre la trace dans Tempo via le label `trace_id`
 3. Copie trace_id   →  depuis Tempo ou la charge utile de l'alerte
 4. Rejeu            →  perf-sentinel tempo --endpoint <url> --trace-id <id>
```

L'étape 4 fait passer la trace historique par le même pipeline `normalize → correlate → detect → score → explain` que le daemon. Vous obtenez les mêmes findings et le même explain tree, mais sur une trace du passé.

**Invocations courantes.**

```bash
# Expliquer une trace identifiée
perf-sentinel tempo --endpoint http://tempo:3200 --trace-id abc123def456

# Balayer un service sur une fenêtre quand le trace_id n'est pas encore connu
perf-sentinel tempo --endpoint http://tempo:3200 --service order-svc --lookback 2h

# Artefact post-mortem pour un ticket ou une PR
perf-sentinel tempo --endpoint http://tempo:3200 --trace-id abc123 --format json > incident.json
```

La sortie SARIF (`--format sarif`) est supportée si votre process incident utilise GitHub Code Scanning.

**Solution de repli : Tempo indisponible.** Si Tempo n'est pas joignable mais que vous avez un dump d'une autre source (export Jaeger/Zipkin, bucket S3 archivé, capture OTLP), passez le fichier directement :

```bash
perf-sentinel explain --input traces-dump.json --trace-id abc123def456
perf-sentinel analyze --input traces-dump.json
```

**Ce qui ne marchera PAS.**

| Tentative                                             | Pourquoi                                    |
|-------------------------------------------------------|---------------------------------------------|
| `curl /api/explain/<trace_id>` sur le daemon live     | Trace évincée après 30 s                    |
| `curl /api/findings` pour reconstruire un explain tree | Le store garde les findings, pas les spans  |
| Attendre que le daemon "refasse remonter" la trace   | Pas de persistance, pas d'endpoint de rejeu |
| Redémarrer le daemon pour retrouver l'état            | Rien n'est persisté sur disque              |

**Prérequis.**

- **Rétention Tempo couvrant la fenêtre de l'incident.** `block_retention` par défaut est de 14 jours mais varie selon le déploiement.
- **Sampling.** Si la trace a été droppée à l'ingestion par un head- ou tail-based sampling, elle a disparu de Tempo aussi. Envisagez un sampling 100 % sur les traces en erreur.
- **Propagation du `trace_id`.** Alertes et logs doivent porter le label. Les exemplars OpenMetrics sur `perf_sentinel_findings_total` et `perf_sentinel_io_waste_ratio` en sont la source la plus directe.

**Option : élargir la fenêtre live.** Si les post-mortems dans le TTL sont fréquents, on échange de la RAM contre du contexte :

```toml
[daemon]
max_active_traces     = 50000    # plafond dur à 1_000_000
trace_ttl_ms          = 300000   # 5 minutes au lieu de 30 secondes
max_retained_findings = 50000
```

---

## Daemon en cours mais inaccessible depuis les clients

**Symptôme.** Le processus du daemon tourne (container up, unité systemd active, les logs indiquent `Starting daemon: gRPC=...:4317, HTTP=...:4318`) mais `curl http://<host>:4318/health` depuis l'extérieur du processus timeout ou est refusé (connection refused).

**Premiers contrôles.**

```bash
# Depuis l'intérieur du container / pod / host qui fait tourner le daemon
# (ça doit toujours marcher) :
curl -sf http://localhost:4318/health

# Depuis l'endroit où vous voulez réellement l'atteindre (c'est celui qui échoue) :
curl -v http://<host>:4318/health

# L'adresse de bind est loguée explicitement au démarrage :
docker logs perf-sentinel 2>&1 | grep 'Starting daemon'
# Attendu : gRPC=0.0.0.0:4317 pour être joignable de l'extérieur. Tout ce
# qui contient 127.0.0.1 est loopback-only et refusera les connexions
# venant d'en dehors du processus.
```

**Causes probables.**

1. **Daemon bindé sur `127.0.0.1` (défaut).** Le listener bind sur l'interface loopback pour des raisons de sécurité. À l'intérieur d'un container, la loopback n'est joignable que *depuis le même container* : un `docker run -p 4318:4318` publie un port au niveau host mais le listener dans le container n'accepte pas la connexion forwardée. Même pattern sur une VM accédée via SSH port-forward ou sur un pod Kubernetes derrière un Service ClusterIP.
2. **`--network host` combiné à des flags `-p`.** En mode host network, le container partage le namespace réseau de l'hôte ; les `-p` sont ignorés et Docker émet `WARNING: Published ports are discarded when using host network mode`. Le daemon n'est joignable que sur l'IP sur laquelle sa config le bind.
3. **Mapping de port inversé ou incomplet.** `docker ps --format '{{.Ports}}'` montre le mapping effectif. Pattern attendu sur un run local de dev : `0.0.0.0:4317-4318->4317-4318/tcp`.
4. **Firewall host, NetworkPolicy ou Security Group cloud qui drop le trafic.** Le `curl` depuis l'intérieur du namespace réseau réussit mais celui de l'extérieur timeout. Si la bind address est `0.0.0.0` et que les logs du daemon n'indiquent pas d'erreur, le delta est environnemental.

**Correctif.**

- Cause (1) : lancer avec `watch --listen-address 0.0.0.0`, ou fixer `[daemon] listen_address = "0.0.0.0"` dans `.perf-sentinel.toml`. Le daemon émettra un warning non-loopback au démarrage, c'est attendu ; placez un reverse proxy ou une NetworkPolicy en amont si c'est un environnement partagé. Voir le quickstart Docker dans [README-FR.md](../../README-FR.md) et les topologies sidecar/collector dans [INTEGRATION-FR.md](INTEGRATION-FR.md).
- Cause (2) : retirer les flags `-p` en mode `--network host` (ils sont ignorés) et s'assurer que le daemon bind sur `0.0.0.0`. Ou revenir au réseau bridge par défaut + `-p` explicites.
- Cause (3) : recréer le container avec l'ordre `-p HOST:CONTAINER` correct.
- Cause (4) : comparer `curl` depuis l'intérieur (réussit) et depuis l'extérieur (échoue). Si le delta est infra, faire remonter la règle bloquante au owner infra.

---

## Aucune trace ingérée

**Symptôme.** `perf_sentinel_events_processed_total` et `perf_sentinel_traces_analyzed_total` restent à zéro. `/api/status` renvoie `active_traces: 0`.

**Premiers contrôles.**

```bash
# Le daemon écoute-t-il sur les ports attendus ?
kubectl logs deploy/perf-sentinel | grep -i "listening on"
# Attendu : "OTLP gRPC listening on 0.0.0.0:4317"
#           "OTLP HTTP listening on 0.0.0.0:4318"

# Depuis un container de service, peut-on joindre le daemon ?
curl -sf http://perf-sentinel:4318/metrics
```

**Causes probables, par ordre.**

1. **Adresse de bind.** Le daemon écoute par défaut sur `127.0.0.1`, injoignable depuis d'autres containers. Mettez `listen_address = "0.0.0.0"` dans `.perf-sentinel.toml` et redémarrez.
2. **Protocole mal aligné.** L'OTel Java Agent utilise gRPC par défaut sur le port 4317. Vérifiez que `OTEL_EXPORTER_OTLP_PROTOCOL` correspond au port visé : `grpc` → 4317, `http/protobuf` → 4318.
3. **Politique réseau.** Un `NetworkPolicy` Kubernetes ou un security group peut bloquer le trafic cross-namespace. Désactivez temporairement ou autorisez explicitement le chemin service → daemon.
4. **Service non instrumenté.** Vérifiez `OTEL_SDK_DISABLED=false` et que le service produit bien des spans (la plupart des SDKs OTel ont des compteurs internes ou des logs debug).
5. **Faute de frappe sur l'endpoint OTLP.** `OTEL_EXPORTER_OTLP_ENDPOINT` doit être `http://<host>:4318`. Pas de suffixe `/v1/traces`, le SDK l'ajoute.

**Vérification après correctif.** Déclenchez une requête via un service instrumenté et observez :

```bash
watch -n 1 'curl -s http://perf-sentinel:4318/metrics | grep events_processed_total'
```

Le compteur doit incrémenter en quelques secondes.

---

## Chute soudaine du volume d'ingestion

**Symptôme.** `rate(perf_sentinel_events_processed_total[5m])` tombe brutalement ou à zéro alors que le daemon est toujours vivant (uptime continue d'augmenter).

**Premiers contrôles.**

```bash
# Confirmer que le daemon est encore vivant, élimine le crash
curl -s http://perf-sentinel:4318/api/status | jq '{uptime_seconds, active_traces}'
```

**Causes probables.**

1. **Trafic amont effondré.** Le trafic réel vers vos services a chuté ; perf-sentinel reflète fidèlement la réalité. Recoupez avec les métriques de votre load balancer ou HTTP.
2. **OTel collector down.** Si un collector central est entre les services et perf-sentinel, vérifiez d'abord sa santé et ses métriques de réception.
3. **Changement de sampling.** Un bump de config a baissé le taux de sampling. Auditez les commits récents dans le repo de config OTel.
4. **Backpressure du daemon.** Si le canal OTLP de réception est plein, les events sont droppés silencieusement. Cherchez `channel full` dans les logs avec `RUST_LOG=sentinel_core::ingest=debug`. Déclencheurs fréquents : pipeline de détection bloqué sur une trace pathologique ; `max_active_traces` trop bas pour le débit courant.

Traitez de haut en bas par élimination. Les cas 1 et 2 représentent la grande majorité.

---

## Spike de findings critiques

**Symptôme.** Alerte sur le rate de `perf_sentinel_findings_total{severity="critical"}`.

**Workflow de triage.**

1. **Grouper par service et type.**

   ```bash
   curl -s 'http://perf-sentinel:4318/api/findings?severity=critical&limit=200' \
     | jq '[.[].finding | {finding_type, service}]
          | group_by(.service, .finding_type)
          | map({key: "\(.[0].service)/\(.[0].finding_type)", count: length})
          | sort_by(-.count)'
   ```

2. **Récupérer un `trace_id` d'exemplar** pour chaque top pattern. Dans Grafana, le ◆ sur la métrique est cliquable ; en ligne de commande :

   ```bash
   curl -s http://perf-sentinel:4318/metrics \
     | grep -E 'findings_total|io_waste_ratio'
   # Les lignes se terminent par "# {trace_id=\"...\"}", copiez cet id
   ```

3. **Expliquer la trace** tant qu'elle est dans la fenêtre live de 30 secondes :

   ```bash
   curl -s http://perf-sentinel:4318/api/explain/<trace_id> | jq .
   ```

   Si évincée, basculez sur [le workflow post-mortem](#analyser-une-trace-plus-ancienne-que-la-fenêtre-live).

4. **Corréler entre services** si l'incident traverse plusieurs équipes :

   ```bash
   curl -s http://perf-sentinel:4318/api/correlations | jq 'sort_by(-.confidence)[:10]'
   ```

**Causes racines courantes.**

- **N+1 SQL :** lazy loading de l'ORM ; une feature récente qui itère sur une collection sans `JOIN FETCH` / `selectinload` / `Include`.
- **Saturation de pool :** pool de connexions sous-dimensionné, ou une dépendance aval qui a ralenti.
- **Requête lente :** index manquant ; un seuil de volume de données franchi (ce qui tournait en 50 ms à 10 k lignes tourne en 2 s à 10 M).

---

## Pression mémoire ou OOM du daemon

**Symptôme.** Le RSS grimpe avec le temps ; OOMKill Kubernetes ; `active_traces` ou `stored_findings` proche des plafonds configurés.

**Premiers contrôles.**

```bash
curl -s http://perf-sentinel:4318/api/status | jq '{active_traces, stored_findings, uptime_seconds}'
# Comparez avec max_active_traces (défaut 10000) et max_retained_findings (défaut 10000) de la config.
```

**Causes probables.**

1. **Trafic au-dessus des valeurs par défaut.** 10 000 traces actives est dimensionné pour une charge modérée. Les services à fort débit remplissent plus vite que l'éviction ne purge.
2. **TTL élargi.** Si vous avez augmenté `trace_ttl_ms` pour la commodité post-mortem, chaque trace vit plus longtemps en mémoire.
3. **Traces pathologiques.** Une seule trace avec des milliers de spans consomme de la RAM. `max_events_per_trace` (défaut 1000) plafonne ; vérifiez qu'il n'a pas été augmenté.
4. **Croissance du correlator.** `[daemon.correlation] max_tracked_pairs` (défaut 10 000) borne le graphe cross-trace. Le relever multiplie la mémoire par le nombre de paires.
5. **Findings store gonflé** par une boucle de détection emballée. Rare mais à vérifier via `stored_findings` vs `max_retained_findings`.

**Correctif.**

```toml
[daemon]
max_active_traces     = 5000     # fenêtre plus petite
trace_ttl_ms          = 30000    # retour au défaut
api_enabled           = false    # désactive l'API de requêtage si non utilisée
max_retained_findings = 0        # court-circuite le ring buffer des findings

[daemon.correlation]
enabled = false                  # skip le correlator pour les daemons mono-service
```

Mettre `max_retained_findings = 0` est le levier le plus efficace pour libérer la RAM quand l'API de requêtage n'est pas consommée. Voir [LIMITATIONS-FR.md](LIMITATIONS-FR.md) § "La mémoire n'est pas libérée par `api_enabled = false` seul".

Redémarrez le daemon pour appliquer. **Pas de hot reload**, voir [Appliquer un changement de config](#appliquer-un-changement-de-config).

---

## Quality gate CI en échec inattendu

**Symptôme.** `perf-sentinel analyze --ci` ou `perf-sentinel tempo --ci` sort avec le code 1. Build rouge.

**Premiers contrôles.**

La sortie JSON contient un bloc `quality_gate` structuré :

```bash
perf-sentinel analyze --ci --input traces.json --format json \
  | jq '.quality_gate.rules[] | select(.passed == false)'
```

Exemple de sortie :

```json
{ "rule": "n_plus_one_sql_critical_max", "threshold": 0, "actual": 2, "passed": false }
```

**Causes probables.**

1. **Régression légitime.** Un changement récent a introduit de nouveaux N+1 ou fait grimper le waste ratio. Inspectez `findings[]` dans le même JSON : `source_endpoint` localise le chemin code ; `pattern.template` montre le SQL/HTTP normalisé ; `pattern.occurrences` donne l'ampleur.
2. **Seuil trop strict.** `.perf-sentinel.toml` peut avoir des tolérances à zéro qui échouent dès qu'un finding préexistant est là. Pour les projets legacy, envisagez un baseline à cliquet (resserrer progressivement plutôt qu'en une fois).
3. **Données de test qui ont grandi.** Un dataset plus large dans les tests d'intégration peut franchir un seuil de détection (un N+1 à 5 occurrences ne se déclenche qu'au-delà d'un certain nombre d'itérations).

**Correctif.** Ajustez soit le code, soit le seuil, pas les deux sous pression. Si le finding est réel, corrigez le code. Si le seuil est mal calibré, mettez à jour `.perf-sentinel.toml` et committez le changement pour qu'il soit relu.

> **Note.** Il n'existe pas de seuils de détection par service à ce jour ; les valeurs `[detection]` s'appliquent globalement à tous les services du fichier de traces.

---

## `perf-sentinel tempo` renvoie 404 ou timeout

**Symptôme.** Soit chaque invocation échoue avec `Tempo returned HTTP 404 for https://.../api/search?...`, soit l'étape search réussit mais la boucle de fetch par trace finit avec `Tempo fetch completed with failures counts={"timeout": N}` et renvoie un résultat partiel (ou vide).

**Premiers contrôles.**

```bash
# Vérifier que l'endpoint est bien une query-frontend Tempo, pas Grafana
# ou un composant interne de Tempo. 200 = OK, 404 = mauvais endpoint.
curl -s -o /dev/null -w 'HTTP %{http_code}\n' \
  '<votre-endpoint>/api/search?limit=1'

# Côté Tempo, surveiller la charge de la query-frontend
kubectl logs -n observability deploy/tempo-query-frontend --tail=50 \
  | grep -E 'error|timeout|queue'
```

**Causes probables.**

1. **Mauvais composant en déploiement microservices.** Dans les déploiements Helm `tempo-distributed`, l'API HTTP de requête est servie exclusivement par `tempo-query-frontend`. Pointer `--endpoint` sur `tempo-querier` (worker interne, pas d'API publique) ou `tempo-ingester` (chemin d'écriture uniquement) renvoie 404 sur chaque `/api/search`. Le message 404 émis par perf-sentinel inclut désormais l'URL qui a échoué pour rendre la mauvaise configuration visible d'un coup d'œil.
2. **Endpoint qui pointe sur Grafana au lieu de Tempo.** Grafana écoute sur 3000 par défaut, l'API HTTP de Tempo sur 3200. `http://grafana:3000/api/search` n'a pas de route correspondante et retourne 404.
3. **Préfixe de reverse proxy oublié.** Si Tempo est derrière un ingress avec un préfixe de path (ex. `https://observability.example.com/tempo/...`), `--endpoint` doit inclure ce préfixe.
4. **Tempo dégradé sous charge de fetch.** Le search a réussi mais les fetches par trace timeout. Déclencheurs courants : `--lookback` long (24 h sur un gros service), `tempo-query-frontend` sous-provisionnée, plafond `max_concurrent_queries` atteint, limites de ressources sur les ingesters (un ingester OOM-killed provoque des échecs de fetch en cascade).

**Correctif.**

- Causes (1), (2), (3) : pointer `--endpoint` sur la vraie URL de query-frontend, validée par le `curl` ci-dessus.
- Cause (4) : côté perf-sentinel, réduire `--lookback` (commencer à 1 h, élargir progressivement) ou basculer sur `--trace-id <id>` pour un replay trace unique. Côté Tempo, scaler `tempo-query-frontend` horizontalement, remonter `max_concurrent_queries`, et vérifier les caps mémoire/CPU des ingesters.

Perf-sentinel plafonne les fetches in-flight à 16 en parallèle par défaut : le client n'inonde pas lui-même Tempo. Si Tempo s'effondre quand même sur un run de 100 traces, c'est la capacité qui bouche, pas le client. Ctrl-C pendant un run long retourne désormais un résultat partiel avec les traces déjà complétées (voir [LIMITATIONS-FR.md](LIMITATIONS-FR.md) § "Ingestion Tempo") ; la CLI renvoie `Tempo fetch was interrupted by Ctrl-C before any trace completed` quand aucune trace n'a eu le temps de se compléter, distinct du `NoTracesFound` générique.

---

## Exemplars absents dans Grafana

**Symptôme.** Les panels affichent les valeurs de métriques mais le marqueur ◆ d'exemplar est absent, ou cliquer dessus ne saute pas vers Tempo.

**Premiers contrôles.**

```bash
# Métriques brutes : chercher "# {trace_id=\"...\"}" en fin de ligne
curl -s http://perf-sentinel:4318/metrics \
  | grep -E 'findings_total|io_waste_ratio'
```

Si les annotations sont présentes dans la sortie brute mais que Grafana ne les rend pas, c'est un problème de config côté Grafana ou Prometheus. Si absentes, perf-sentinel n'a encore enregistré aucun exemplar.

**Causes probables.**

1. **Aucun finding encore.** Les exemplars ne sont posés qu'à la détection. Un daemon à zéro finding n'en a aucun. Déclenchez du trafic sur un chemin qui produit un N+1 ou une requête lente.
2. **Stockage d'exemplars Prometheus non activé.** Prometheus doit être lancé avec `--enable-feature=exemplar-storage`. Vérifiez sur la page des flags Prometheus.
3. **Datasource Grafana pas liée à Tempo.** Dans Grafana → Connections → datasource Prometheus → Exemplars, configurez un exemplar avec `datasourceUid` pointant vers votre datasource Tempo et `labelName: trace_id`.
4. **`trace_id` épuré.** perf-sentinel filtre les valeurs d'exemplar à `[a-zA-Z0-9_-]` et tronque à 64 caractères. Des formats de trace ID inhabituels (UUIDs avec accolades, encodages custom) peuvent être déformés. Voir `sanitize_exemplar_value` dans `report/metrics.rs`.

---

## Scraper d'énergie bloqué

**Symptôme.** `perf_sentinel_scaphandre_last_scrape_age_seconds` ou `perf_sentinel_cloud_energy_last_scrape_age_seconds` grimpe de façon monotone au-delà de l'intervalle de scrape configuré. Les scrapers sains remettent cette gauge près de zéro après chaque scrape réussi.

**Premiers contrôles.**

```bash
curl -s http://perf-sentinel:4318/metrics | grep scrape_age_seconds
```

Activer les logs de scoring pour voir l'échec réel :

```bash
RUST_LOG=sentinel_core::score=debug
# Chercher "scaphandre scrape failed" ou "cloud_energy scrape failed"
```

**Causes probables.**

1. **Permissions du container Scaphandre.** Les compteurs RAPL nécessitent `CAP_SYS_RAWIO`, le mode privileged, ou un hostPath vers `/sys/class/powercap`. Sans ça, les scrapes échouent au niveau des privilèges.
2. **Endpoint injoignable.** Vérifiez l'URL dans `[green.scaphandre] endpoint`. Le réseau entre perf-sentinel et l'exporteur Scaphandre doit être ouvert.
3. **API d'énergie cloud down ou rate-limitée.** Si vous utilisez Electricity Maps ou une API de cloud provider, vérifiez son statut et votre quota API.
4. **Nom de service qui ne correspond pas.** Les clés `[green.cloud.services.<name>]` doivent matcher l'attribut `service.name` des spans entrants. Sans correspondance, pas d'attribution par service.

**Impact.** Le daemon retombe sur le modèle proxy I/O pour les estimations d'énergie. Les chiffres CO₂ restent directionnels mais perdent leur précision de mesure. Ce n'est pas un incident chaud ; à corriger lors de la prochaine fenêtre de maintenance sauf si la précision compte pour un rapport spécifique.

---

## `/api/correlations` renvoie vide

**Symptôme.** Les panels de corrélations cross-trace sont vides alors même que plusieurs services produisent des findings.

**Premiers contrôles.**

```bash
curl -s http://perf-sentinel:4318/api/correlations | jq 'length'
# 0 = aucune corrélation n'a passé les seuils
```

**Causes probables.**

1. **Correlator désactivé.** Le défaut est `[daemon.correlation] enabled = false`. Activez-le.
2. **Seuils trop stricts.** Défauts :
   - `min_co_occurrences = 5` : il faut 5 incidents conjoints avant qu'une paire soit considérée
   - `min_confidence = 0.7` : 70 % de confiance sur la corrélation
   - `lag_threshold_ms = 5000` : fenêtre de 5 secondes entre cause et effet

   Des pics de trafic courts accumulent rarement 5 co-occurrences. Baissez pour dev/staging, gardez conservateur en prod.
3. **Services légitimement indépendants.** Des services découplés sains ne produisent aucune corrélation. L'absence n'est pas toujours un bug.

**Correctif.**

```toml
[daemon.correlation]
enabled            = true
min_co_occurrences = 3
min_confidence     = 0.6
lag_threshold_ms   = 10000
max_tracked_pairs  = 20000
```

Redémarrez le daemon pour appliquer.

---

## `/api/export/report` retourne 503 ou un rapport vide

**Symptôme.** Piper le daemon vers le dashboard HTML échoue avec HTTP 503, ou produit un dashboard à zéro findings sur un daemon qui tourne manifestement.

```bash
curl -s http://perf-sentinel:4318/api/export/report | perf-sentinel report --input - --output /tmp/report.html
# HTTP 503: {"error": "daemon has not yet processed any events"}
```

**Causes probables.**

1. **Cold start.** L'endpoint retourne 503 tant que `events_processed > 0` n'est pas vrai, volontairement : rendre un dashboard avec des compteurs à zéro sur un daemon qui n'a pas encore vu son premier batch OTLP serait trompeur. Attends le premier batch, puis réessaie. `GET /api/status` expose le compteur `events_processed` live.
2. **`api_enabled = false`.** Si la config désactive la query API, `/api/export/report` n'est pas monté et `curl` retourne un 404, pas un 503. Réactive `[daemon] api_enabled = true`.
3. **Findings store vide, pas cold start.** Sur un daemon long-running qui a traité des events mais qui n'a aucun finding dans le ring buffer (trafic clean, ou `max_retained_findings = 0`), l'endpoint retourne 200 avec un tableau `findings` vide. Le dashboard résultant affiche un état "No findings", ce qui est correct.

**Note opérationnelle.** Le snapshot n'est pas atomique entre `findings` et `correlations` : les deux collections peuvent être décalées d'un batch (findings de la génération N, correlations de N+1). Pour un dashboard post-mortem c'est acceptable. Si tu as besoin d'une cohérence stricte, utilise `analyze --input traces.json` sur un fichier de traces capturé à la place.

---

## Crash ou redémarrage du daemon

**Symptôme.** Le processus du daemon s'est arrêté (OOM kernel, panic, éviction du pod, rollout de déploiement).

**Ce qui est perdu.**

- Toutes les traces de la fenêtre glissante (jusqu'à `max_active_traces`).
- Tous les findings retenus (jusqu'à `max_retained_findings`).
- L'état de corrélation cross-trace.
- Le compteur d'uptime est réinitialisé.

**Ce qui survit.**

- Rien du daemon lui-même. Pas de persistance disque.
- Prometheus conserve les métriques déjà scrapées (les compteurs historiques sont saufs).
- Tempo conserve les traces, à condition que vous les y envoyiez aussi.

**Récupération.**

1. Démarrer un nouveau daemon avec la même config.
2. Attendre que les collectors / SDKs OTel se reconnectent. Les clients OTel retry avec backoff exponentiel. Comptez jusqu'à ~60 secondes avant que l'ingestion ne reprenne pleinement.
3. Pour les incidents survenus *pendant* l'interruption, utilisez [le workflow post-mortem](#analyser-une-trace-plus-ancienne-que-la-fenêtre-live) contre Tempo.

**Prévention.**

- Kubernetes `restartPolicy: Always` + marge de limit mémoire au-dessus du RSS pic observé.
- Alertez sur `perf_sentinel_active_traces` approchant `max_active_traces`. La pression montante précède souvent l'OOM.
- Pour la HA, lancez plusieurs replicas derrière un load balancer. Chaque replica a un état indépendant (pas de corrélation cross-replica), mais l'ingestion devient redondante face à une panne single-instance.

---

## Appliquer un changement de config

**Le daemon ne recharge pas `.perf-sentinel.toml` à chaud.** Toute édition de config nécessite un redémarrage :

```bash
# Kubernetes
kubectl rollout restart deployment/perf-sentinel

# systemd
systemctl restart perf-sentinel

# Docker
docker restart perf-sentinel
```

Comptez une brève interruption de l'ingestion (quelques secondes à une minute) pilotée par le comportement de retry des SDKs OTel. Pour les tuning non urgents, profitez d'une fenêtre de déploiement normale.

**Valider avant le rollout.** Le daemon parse le TOML au démarrage et quitte avec une erreur claire sur entrée malformée. Smoke-testez la config candidate dans un daemon jetable d'abord :

```bash
perf-sentinel watch --config /path/to/candidate-config.toml
# Sort immédiatement sur erreur de parsing en imprimant la ligne fautive.
```

Une fois qu'il démarre proprement, déployez en production.

---

## Voir aussi

- [LIMITATIONS-FR.md](LIMITATIONS-FR.md) : ce que le daemon ne persiste pas et ne garantit pas.
- [QUERY-API-FR.md](QUERY-API-FR.md) : référence `/api/findings`, `/api/explain`, `/api/correlations`, `/api/status`.
- [INTEGRATION-FR.md](INTEGRATION-FR.md) : mise en place de bout en bout, quatre topologies supportées, intégration Tempo et Jaeger. Voir [INSTRUMENTATION-FR.md](INSTRUMENTATION-FR.md) pour le câblage OTLP par langage et [CI-FR.md](CI-FR.md) pour les recettes d'intégration CI.
- [CONFIGURATION-FR.md](CONFIGURATION-FR.md) : référence complète `[daemon]`, `[detection]`, `[green]`, `[daemon.correlation]`.
