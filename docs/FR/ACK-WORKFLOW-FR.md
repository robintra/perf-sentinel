# Workflow d'acquittement

perf-sentinel supporte deux mécanismes d'acquittement complémentaires :
TOML in-repo (CI ack, depuis 0.5.17) et JSONL via l'API HTTP du
daemon (daemon ack, depuis 0.5.20). Ils couvrent des scénarios
opérationnels différents et peuvent cohabiter. Cette page explique
comment chacun fonctionne, quand choisir lequel, et comment le helper
CLI introduit en 0.5.22 s'intègre côté daemon.

## CI ack : TOML dans le repo

Le fichier `.perf-sentinel-acknowledgments.toml` à la racine d'un
repository applicatif, versionné dans git, modifié via revue de PR.
À utiliser pour les décisions permanentes prises par l'équipe : faux
positifs, findings à risque accepté connus, choix de design
intentionnels.

### Ajouter un ack TOML

Éditer le fichier directement :

```toml
[[acknowledged]]
signature = "n_plus_one_sql:order-svc:_api_orders:0123456789abcdef"
acknowledged_by = "team-architecture"
acknowledged_at = "2026-05-04T13:30:00Z"
reason = "Fan-out intentionnel pour endpoint de reporting batch"
```

Commit, ouvrir une pull request, faire reviewer, merger. Le prochain
run CI honorera l'ack via `analyze --acknowledgments` et les
[templates CI](../ci-templates) livrés avec le projet.

### Retirer un ack TOML

Supprimer l'entrée, commit, PR, revue, merge. Même cycle de vie que
l'ajout.

## Daemon ack : JSONL via API

Pour les acks temporaires en runtime, faits par les SRE ou l'oncall :
différer un finding pendant qu'un fix est livré, supprimer du bruit
pendant un incident connu, etc. Le daemon persiste ces acks dans un
fichier JSONL en append-only events, avec timestamps d'expiration
optionnels.

### Ajouter un ack daemon via curl (bas-niveau)

```bash
curl -X POST http://daemon:4318/api/findings/<sig>/ack \
  -H "Content-Type: application/json" \
  -d '{"by":"alice","reason":"reporté","expires_at":"2026-05-11T00:00:00Z"}'
```

Quand l'auth est activée côté serveur (`[daemon.ack] api_key`),
ajouter `-H "X-API-Key: <CLÉ>"`.

### Ajouter un ack daemon via le CLI (depuis 0.5.22, recommandé)

```bash
perf-sentinel ack create \
  --signature "n_plus_one_sql:order-svc:_api_orders:0123456789abcdef" \
  --reason "reporté au prochain sprint" \
  --expires 7d
```

Le CLI gère la résolution de l'auth, le parsing de durée (relative
ou ISO8601), la résolution de l'URL daemon, et produit des messages
d'erreur lisibles. Voir [`CLI-FR.md`](./CLI-FR.md#ack) pour la
référence complète, y compris les caps de 1 KiB appliqués aux
signatures lues sur stdin et au prompt API-key interactif.

### Révoquer un ack daemon

```bash
perf-sentinel ack revoke \
  --signature "n_plus_one_sql:order-svc:_api_orders:0123456789abcdef"
```

Ou via curl :

```bash
curl -X DELETE http://daemon:4318/api/findings/<sig>/ack
```

## Lister les acks actifs

```bash
perf-sentinel ack list                  # acks daemon, format table
perf-sentinel ack list --output json    # acks daemon, JSON
```

`perf-sentinel ack list` n'énumère que les acks côté daemon. Les
acks TOML CI vivent dans le fichier lui-même, à consulter avec :

```bash
cat .perf-sentinel-acknowledgments.toml
```

## Interop : TOML gagne en cas de conflit

Les deux sources sont fusionnées au moment du filtrage des findings.
Si la même signature est ack dans TOML et dans le JSONL daemon, la
version TOML l'emporte. Rationale : la baseline TOML est livrée via
revue de PR et représente une décision immuable au niveau équipe, le
JSONL daemon est un override mutable, runtime-only.

Un `POST /api/findings/{sig}/ack` sur une signature déjà couverte par
TOML retourne HTTP 409 pour éviter un shadowing silencieux. Le CLI
`ack create` mappe ça à exit 2 avec un hint qui pointe vers
`ack revoke`.

### Ajouter un ack daemon depuis le rapport HTML (depuis 0.5.23, navigateur)

Le rapport HTML peut tourner en mode live et piloter les mêmes
endpoints daemon depuis le navigateur. Générez le rapport avec
`--daemon-url`, ouvrez-le, cliquez sur le bouton `Ack` à côté de
chaque finding. Voir [`HTML-REPORT-FR.md`](./HTML-REPORT-FR.md) pour
la configuration, les prérequis CORS et la gestion de la clé X-API-Key.

```bash
perf-sentinel report --input traces.json --output report.html \
  --daemon-url http://localhost:4318
open report.html
```

### Ajouter un ack daemon depuis le TUI (depuis 0.5.24, terminal)

`perf-sentinel query inspect` ouvre un TUI interactif qui expose la
liste des findings du daemon, les arbres de spans et les corrélations
cross-trace. Avec 0.5.24, presser `a` sur le finding sélectionné ouvre
une modale d'acknowledgment (raison, expires, by) qui poste sur le
même endpoint daemon. `u` ouvre une modale de confirmation de revoke.
Le panel Findings affiche un indicateur italique gris `[acked by
<user>]` à droite des findings déjà acknowledged. Voir
[`INSPECT-FR.md`](./INSPECT-FR.md) pour la liste des keybindings et
le flow d'auth.

```bash
perf-sentinel query --daemon http://localhost:4318 inspect
# Press 'a' sur un finding, modale, remplir reason, Tab vers Submit, Enter
```

`a` et `u` sont no-op en mode batch (`inspect --input`) puisque
l'acknowledgment a besoin d'un daemon qui tourne pour persister.

## Choisir entre TOML et daemon

| Scénario                                          | Utiliser                                |
| ------------------------------------------------- | --------------------------------------- |
| Décision permanente par l'équipe                  | TOML (versionné, auditable git)         |
| Report temporaire pendant un incident             | Daemon (CLI ou curl)                    |
| Faux positif partagé par tous les environments    | TOML                                    |
| Suppression spécifique à un environment           | Daemon (un par environment)             |
| Nettoyage onboarding sur findings préexistants    | TOML (en bulk via éditeur)              |
| Ack ponctuel à 3h du matin via PagerDuty          | CLI daemon                              |
| Clic Ack depuis le rapport CI en revue de MR      | Daemon (mode live HTML, depuis 0.5.23)  |
| Audit des findings depuis une session terminal    | Daemon (TUI, depuis 0.5.24)             |

## Observabilité

Le daemon expose des compteurs Prometheus sur `/metrics` pour chaque
opération ack qu'il traite
(`perf_sentinel_ack_operations_total{action}` et
`perf_sentinel_ack_operations_failed_total{action,reason}`). Voir
[`METRICS-FR.md`](./METRICS-FR.md) pour le schéma complet et des
exemples PromQL.

## Stabilité de signature et redémarrages de service

Les acks matchent les findings via une signature canonique :

```
<finding_type>:<service>:<endpoint_sanitisé>:<préfixe-sha256-du-template>
```

La signature exclut volontairement `trace_id` et `span_id`, donc un
ack unique survit aux redémarrages de service et au trafic normal
porteur d'identifiants de requête variables. Le contrat est verrouillé
par les tests unitaires dans
`crates/sentinel-core/src/acknowledgments.rs`.

### Dépendance critique à `http.route`

Le composant `endpoint` est dérivé de l'attribut OpenTelemetry
`http.route` sur le span HTTP parent, qui porte le template de route
(par exemple `/api/orders/{id}`) plutôt que l'URL instanciée
(`/api/orders/42`).

Quand les services tracés émettent `http.route` :

- Le même finding sur le même endpoint logique produit la même
  signature.
- Les acks survivent aux redémarrages de service.
- Les acks survivent au trafic normal avec des identifiants de
  requête tournants.

Quand `http.route` est absent, perf-sentinel se rabat sur `http.url`,
puis sur `url.full` (convention OTel stable v1.21+). Chaque URL
unique produit une signature différente, le churn d'acks devient
proportionnel à la cardinalité des URL, et les findings différés
réapparaissent à chaque nouvel id de requête. Le fallback existe pour
fournir une chaîne d'endpoint exploitable, pas comme posture
recommandée.

Les agents OpenTelemetry standard émettent `http.route`
automatiquement :

- Spring Boot 3+ avec l'agent Java OpenTelemetry.
- ASP.NET Core avec le SDK .NET OpenTelemetry.
- Express.js, Fastify, Koa avec `@opentelemetry/instrumentation-*`.
- La plupart des auto-instrumentations de frameworks HTTP modernes.

Pour vérifier qu'un service instrumenté émet bien des templates de
route, inspecter le `source_endpoint` d'un finding récent contre un
daemon qui tourne :

```bash
curl -s http://localhost:4318/api/findings | jq -r '.[].source_endpoint' | sort -u
```

Des templates avec placeholders (`/api/orders/{id}`) signalent une
instrumentation saine. Des URL instanciées avec des id en dur
(`/api/orders/42`) signalent que `http.route` est absent et que les
acks vont churner.

### Périmètre du carbon scoring

Le champ `green_impact` sur chaque finding est calculé par détection
au sein d'une seule trace. Les valeurs reportées par
`perf-sentinel analyze` ou dans le rapport JSON décrivent une
occurrence et n'agrègent pas cross-traces.

Le daemon expose des compteurs Prometheus
(`perf_sentinel_findings_total`,
`perf_sentinel_avoidable_io_ops_total`) qui accumulent de manière
monotone sur la durée de vie du daemon. Chaque batch contribue avec
sa propre dédup intra-batch, clé sur
`(trace_id, template, source_endpoint)`, ce qui empêche de compter
deux fois le même pattern dans un seul batch. Les traces distinctes,
y compris celles produites après un redémarrage de service,
contribuent séparément parce qu'elles représentent des exécutions de
requête distinctes. Les compteurs ne se réinitialisent qu'au
redémarrage du process daemon, conformément à la sémantique standard
des counters Prometheus. Préférer `rate(...)` sur des fenêtres
courtes pour les dashboards de tendance plutôt que de lire la valeur
absolue brute.
