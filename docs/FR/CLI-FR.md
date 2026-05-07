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
  --signature "n_plus_one_sql:order-svc:_api_orders:0123456789abcdef" \
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
  --signature "n_plus_one_sql:order-svc:_api_orders:0123456789abcdef"
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

Pour l'instant, voir `perf-sentinel <subcommand> --help` pour la
liste complète des options de `analyze`, `watch`, `query`, `report`,
`diff`, `explain`, `inspect`, `pg-stat`, `tempo`, `jaeger-query`,
`demo`, `bench` et `calibrate`. Les commandes elles-mêmes sont
stables, leur documentation prose est complétée incrémentalement.

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
