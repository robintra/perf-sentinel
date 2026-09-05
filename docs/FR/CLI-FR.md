# Référence CLI

Cette page documente les sous-commandes utilisateur du binaire
`perf-sentinel`. Pour les notes d'architecture et de design, voir
[`ARCHITECTURE-FR.md`](./ARCHITECTURE-FR.md). Pour les hooks d'exécution
(quality gates CI, codes de sortie, variables d'environnement), voir
[`CI-FR.md`](./CI-FR.md) et [`RUNBOOK-FR.md`](./RUNBOOK-FR.md).

L'inventaire complet des options est aussi accessible via `--help` sur
chaque sous-commande :

```bash
perf-sentinel --help
perf-sentinel <subcommand> --help
```

Les sections ci-dessous ne sont pas exhaustives pour chaque
sous-commande, elles se concentrent sur les surfaces utilisateur qui
bénéficient d'une explication en prose (workflow, valeurs par défaut,
codes de sortie). Pour la liste complète des flags, préférez `--help`.

## capture

Reçoit des traces OTLP et les écrit dans un fichier, pour qu'un job de
CI produise l'entrée sur laquelle `analyze --ci` pose sa gate sans faire
tourner d'OpenTelemetry Collector. Cette commande reçoit et écrit, elle
n'analyse jamais : le verdict reste à `analyze`, sur un fichier que vous
pouvez conserver comme artefact de build, rejouer avec d'autres seuils,
ou comparer à une référence avec `diff`.

L'application n'a besoin d'aucun réglage propre à perf-sentinel, juste
des variables d'endpoint standard. Précisez aussi le protocole : les SDK
ne s'accordent pas sur leur défaut, et un endpoint pointé sur le mauvais
n'exporte rien sans le dire.

```bash
# OTLP HTTP, le défaut de la plupart des SDK
export OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318
export OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf

# OTLP gRPC, le défaut d'opentelemetry-java
export OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317
export OTEL_EXPORTER_OTLP_PROTOCOL=grpc
```

### Deux formes

**Envelopper l'étape de test**, la plus solide :

```bash
perf-sentinel capture --output traces.json -- mvn verify
perf-sentinel analyze --ci --input traces.json
```

Les ports sont liés avant que la commande démarre, aucun export ne se
perd donc dans une course au démarrage, et la capture s'arrête à la fin
de la commande plutôt que sur un délai deviné. La commande hérite de
stdout et stderr sans altération, et son code de sortie est propagé :
un échec de tests reste un échec de job.

La commande enveloppée est celle qui lance vos tests, quelle qu'elle
soit. `capture` démarre un processus, peu lui importe lequel, et rien
là-dedans n'est propre à Java :

```bash
perf-sentinel capture --output traces.json -- ./gradlew integrationTest
perf-sentinel capture --output traces.json -- pytest tests/integration
perf-sentinel capture --output traces.json -- npm run test:e2e
perf-sentinel capture --output traces.json -- go test ./...
perf-sentinel capture --output traces.json -- dotnet test
perf-sentinel capture --output traces.json -- ./scripts/run-integration-tests.sh
```

La commande est exécutée directement, sans passer par un shell : un pipe
ou une chaîne `&&` doit vivre dans un script que `capture` enveloppe
ensuite.

Ce qui change d'un langage à l'autre, c'est l'instrumentation qui fait
exporter l'application, décrite dans
[`INSTRUMENTATION-FR.md`](INSTRUMENTATION-FR.md). `capture` ne fait
qu'écouter.

**Tests dans des conteneurs.** Quand l'application testée tourne dans un
conteneur voisin plutôt que sur le runner, liez l'écoute sur toutes les
interfaces et pointez l'exporteur vers l'hôte :

```bash
perf-sentinel capture --output traces.json --listen-address 0.0.0.0 -- docker compose up --exit-code-from tests
# dans le conteneur : OTEL_EXPORTER_OTLP_ENDPOINT=http://host.docker.internal:4318
#                     OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf
```

**Écouter à côté d'une étape de test existante**, quand votre pipeline
génère la commande de test et qu'elle ne peut pas être préfixée :

```bash
perf-sentinel capture --output traces.json &
CAPTURE=$!
./scripts/run-integration-tests.sh
kill -TERM $CAPTURE && wait $CAPTURE
```

> **Préfixez l'étape existante, n'en ajoutez jamais une seconde.**
> `capture -- mvn verify` lance les tests une fois. Une nouvelle étape
> de pipeline à côté de l'existante ferait tourner toute la suite
> d'intégration deux fois, pour rien.

### Sortie

Du NDJSON, une requête OTLP par ligne, la forme que produit l'exporteur
`file` du Collector. `analyze`, `report` et `diff` la détectent
automatiquement, sans flag. Les requêtes sont écrites telles que reçues,
sans conversion, le fichier décrit donc ce que l'application a réellement
envoyé.

Le répertoire de `--output` est créé s'il manque, de sorte que
`--output target/traces.json` fonctionne sur un workspace CI vierge où
l'outil de build n'a pas encore créé `target/`. En mode enveloppe, cet
outil de build est justement celui qui n'a pas encore tourné.

La progression et le décompte final vont sur **stderr**, jamais sur
stdout : en mode enveloppe, ce flux appartient à la commande enveloppée.
Ce résumé est ce qui permet de distinguer "aucun anti-pattern trouvé" de
"rien n'a jamais été exporté", et un fichier de traces vide est rejeté
par `analyze` au lieu d'être présenté comme une gate au vert.

### Options utiles

| Flag | Défaut | Pourquoi le changer |
|---|---|---|
| `--listen-address` | `127.0.0.1` | `0.0.0.0` quand l'application tourne dans un autre conteneur du même job |
| `--listen-port-grpc` / `--listen-port-http` | `4317` / `4318` | un port est déjà pris sur l'agent |
| `--max-file-size` | `512` (Mio) | une grosse suite. Au-delà du plafond le fichier reste valide mais incomplet, et le run sort en `2` plutôt que de faire semblant |
| `--grace-ms` | `2000` | combien de temps continuer à écouter après la fin de la commande, pour le dernier flush de l'exporteur |

### Codes de sortie

- `0` : capture terminée.
- le code de la commande enveloppée quand elle a échoué, ce signal étant
  le plus important. Une commande tuée par un signal renvoie
  `128 + signal`, comme le ferait un shell, jamais `0`.
- `1` : la capture elle-même a échoué (port pris, fichier non
  inscriptible). Les deux sont détectés avant le démarrage de la commande
  enveloppée, une capture qui ne peut pas écouter ne laisse donc jamais
  une suite de tests en cours.
- `2` : le fichier de traces est en deçà du run, soit parce que le
  plafond de taille a été atteint, soit parce que des requêtes n'ont pas
  pu être mises en file assez vite. Tout verdict qui en sortirait
  sous-estimerait le run.

## ack

Acquitter des findings via l'API daemon ack introduite en 0.5.20.
Trois sous-actions : `create`, `revoke`, `list`.

Le CLI consomme les endpoints HTTP du daemon
(`POST/DELETE /api/findings/{sig}/ack` et `GET /api/acks`). Il ne
modifie pas la baseline TOML CI
(`.perf-sentinel-acknowledgments.toml`) qui est faite pour être éditée
à la main et livrée via revue de PR. Voir
[`ACK-WORKFLOW-FR.md`](./ACK-WORKFLOW-FR.md) pour choisir entre les
deux mécanismes.

### Synopsis

```bash
perf-sentinel ack [OPTIONS] <SUBCOMMAND>
```

Options de niveau supérieur (s'appliquent aux trois sous-actions) :

- `--daemon <URL>` : endpoint HTTP du daemon. Par défaut
  `$PERF_SENTINEL_DAEMON_URL` puis `http://localhost:4318`.

### `ack create`

Créer un nouvel acquittement.

```bash
perf-sentinel ack create \
  --signature "n_plus_one_sql:order-svc:_api_orders:0123456789abcdef0123456789abcdef" \
  --reason "reporté au prochain sprint" \
  --expires 7d
```

Options :

- `--signature <SIG>` (ou `-s`) : signature du finding à acquitter. Si
  omis, le CLI lit la signature depuis stdin (uniquement quand stdin
  n'est pas un TTY). La lecture stdin est plafonnée à 1 KiB, un pipe
  `cat /dev/urandom` ne peut donc pas saturer la mémoire avant que le
  validateur côté daemon rejette l'entrée.
- `--reason <TEXTE>` (ou `-r`) : requis, description libre de la
  raison de l'acquittement.
- `--expires <ISO8601_OR_DURATION>` : expiration de l'ack. Accepte un
  datetime ISO8601 (`2026-05-11T00:00:00Z`) ou une durée relative
  (`7d`, `24h`, `30m`). Omettre pour un ack permanent.
- `--by <NOM>` : identité de la personne qui acquitte. Fallback sur
  `$USER`, puis `"anonymous"`.
- `--api-key-file <CHEMIN>` : voir "Authentification" plus bas.

### `ack revoke`

Retirer un acquittement existant.

```bash
perf-sentinel ack revoke \
  --signature "n_plus_one_sql:order-svc:_api_orders:0123456789abcdef0123456789abcdef"
```

### `ack list`

Énumérer les acquittements daemon actifs.

```bash
perf-sentinel ack list
perf-sentinel ack list --output json
```

`ack list` ne montre que les acks daemon. Les acks TOML CI restent
visibles directement dans
`.perf-sentinel-acknowledgments.toml`. Le daemon plafonne la réponse
à 1000 entrées.

### Authentification

Quand le daemon impose une clé API (`[daemon.ack] api_key` côté
config), le CLI la résout dans cet ordre :

1. Variable d'environnement `PERF_SENTINEL_DAEMON_API_KEY`.
2. `--api-key-file <CHEMIN>`. Le contenu du fichier est lu et tout
   newline final est strippé.
3. Prompt interactif `rpassword` (sans écho) si le daemon retourne
   401 et stdin est un TTY. La valeur collée est plafonnée à 1 KiB.

`query inspect`, `query monitor` et `query incidents` prennent le même
`--api-key-file <CHEMIN>` et le résolvent de la même façon (étapes 1 et
2, pas de prompt). La clé de lecture `[daemon] read_api_key` suffit pour
`query monitor` et `query incidents`, `ack` et `query inspect` ont
besoin de la clé d'ack.

Il n'y a pas de flag `--api-key <SECRET>` direct, par design : passer
des secrets en ligne de commande les expose via la liste des
processus et l'historique du shell.

Sur Unix, `--api-key-file` est ouvert avec `O_NOFOLLOW` (les liens
symboliques sont refusés) et le CLI affiche un avertissement d'une
ligne sur stderr si le fichier est lisible par le groupe ou tous
(`mode & 0o077 != 0`). L'avertissement n'est émis que si stderr est
un TTY : dans les contextes CI / Docker / systemd où stderr n'est pas
un TTY, l'avertissement est supprimé pour garder les logs de build
propres. Les opérateurs dans ces environnements doivent fixer les
permissions du fichier de manière déclarative (k8s Secret avec
`defaultMode: 0o400`, `StatefulSet` monté depuis un `Secret`, etc.)
plutôt que de compter sur l'avertissement runtime.

### Résolution de l'URL du daemon

`--daemon <URL>` > variable `PERF_SENTINEL_DAEMON_URL` > défaut
`http://localhost:4318`. Le défaut correspond à `perf-sentinel watch`,
qui écoute sur le port standard OTLP/HTTP.

### Codes de sortie

- `0` : succès.
- `1` : erreur générique (réseau, parse, signature absente sur stdin).
- `2` : erreur client (HTTP 4xx). Inclut 401 (non autorisé), 409
  (déjà acquitté), 404 (non acquitté sur revoke), 400 (signature
  invalide).
- `3` : erreur serveur (HTTP 5xx). Inclut 503 (store ack désactivé),
  500 (échec d'écriture) et 507 (store ack plein).

Les erreurs sont écrites sur stderr avec une cause sur une ligne et
un hint actionnable quand pertinent.

## Autres sous-commandes

`perf-sentinel query incidents [--service <NOM>] [--offset N] [--limit N]
[--format text|json] [--api-key-file <CHEMIN>]` liste les incidents que
l'alerting a postés au daemon (0.20.0+, `[daemon.incidents]`), du plus
récent au plus ancien : un bloc d'en-tête par incident (genre, service,
début et fin en heure locale, la fenêtre, un marqueur de capture qui dit
si le ring détenait encore toute la fenêtre, combien de findings n'ont
brûlé qu'après le redémarrage, le détail de l'alerte et l'identifiant),
puis ses findings dans la mise en page de `query findings`. `--limit`
vaut 50 par défaut, le daemon le plafonne à 100, `--offset` pagine
au-delà des plus récents. Un 401 (passez la clé), un 503
(`[daemon.incidents] enabled = false`) et un 404 (daemon antérieur à
0.20.0) sortent chacun en 1 avec leur cause nommée. `query monitor
--api-key-file <CHEMIN>` remet la même clé à l'onglet Incidents du
moniteur, voir [`INSPECT-FR.md`](./INSPECT-FR.md).

Pour l'instant, voir `perf-sentinel <subcommand> --help` pour la
liste complète des options de `analyze`, `watch`, `query`, `report`,
`diff`, `explain`, `inspect`, `pg-stat`, `mysql-stat`, `tempo`, `jaeger-query`,
`demo`, `bench` et `calibrate`. Les commandes elles-mêmes sont
stables, leur documentation prose est complétée incrémentalement.

Le trio chaîne d'approvisionnement dispose d'une documentation
dédiée ailleurs : `disclose`, `verify-hash` et `hash-bake` sont
couverts dans [`REPORTING-FR.md`](./REPORTING-FR.md), avec le
contexte signature et provenance dans
[`SUPPLY-CHAIN-FR.md`](./SUPPLY-CHAIN-FR.md). La complétion shell et
la page de manuel sont documentées plus bas sur cette page.

## Complétion shell

`perf-sentinel completions <shell>` écrit un script de complétion sur
stdout. Shells supportés : `bash`, `zsh`, `fish`, `powershell`,
`elvish`. Rediriger la sortie vers le chemin de complétion du shell :

```bash
# Zsh (oh-my-zsh, prezto, fpath manuel)
perf-sentinel completions zsh > ~/.zfunc/_perf-sentinel

# Bash
perf-sentinel completions bash > /usr/local/etc/bash_completion.d/perf-sentinel

# Fish
perf-sentinel completions fish > ~/.config/fish/completions/perf-sentinel.fish
```

Recharger le shell, ou `source` le fichier, après l'installation.
Régénérer le script après chaque upgrade de `perf-sentinel` pour que
la complétion reste alignée avec les nouveaux flags et sous-commandes.

## Page de manuel

`perf-sentinel man` écrit une page de manuel roff sur stdout. Elle rend
la page de premier niveau, qui liste les sous-commandes (comme `git.1`).
Rediriger la sortie vers le chemin des pages de manuel :

```bash
perf-sentinel man > /usr/local/share/man/man1/perf-sentinel.1
```

`man perf-sentinel` fonctionne ensuite. Pour prévisualiser sans
installer :

```bash
perf-sentinel man > /tmp/perf-sentinel.1 && man /tmp/perf-sentinel.1
```

Régénérer la page après chaque upgrade de `perf-sentinel` pour qu'elle
reste alignée avec les nouveaux flags et sous-commandes.
