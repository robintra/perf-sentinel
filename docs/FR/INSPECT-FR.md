# Inspecteur interactif (TUI)

`perf-sentinel` embarque un TUI interactif pour explorer les
findings, les arbres de spans et les corrélations cross-trace. Deux
points d'entrée :

- `perf-sentinel inspect --input <events.json>` : mode batch, lit un
  fichier d'events brut ou un JSON Report pré-calculé.
- `perf-sentinel query --daemon <URL> inspect` : mode live, lit les
  findings et les traces depuis un daemon en cours d'exécution via HTTP.

En mode live (0.5.24+), le TUI permet aussi à l'opérateur d'acknowledger
et de révoquer des findings interactivement depuis le terminal.

## Layout

L'écran se découpe en une layout 2 lignes :

```
┌─ Traces ──┬─ Findings ────────────────┬─ Correlations ────┐
│ trace-1   │ [1] N+1 SQL CRITICAL      │ svc-a -> svc-b    │
│ trace-2   │ [2] Redundant SQL WARNING │ ...               │
│ ...       │ [3] Slow HTTP INFO        │                   │
├───────────┴───────────────────────────┴───────────────────┤
│ Detail (largeur complète, arbre de spans + métadonnées)   │
└───────────────────────────────────────────────────────────┘
```

La bordure du panel actif est cyan, le reste reste gris.

## Keybindings

| Touche         | Action                                          |
|----------------|-------------------------------------------------|
| `q`            | Quitter                                         |
| `↑` / `k`      | Sélection vers le haut                          |
| `↓` / `j`      | Sélection vers le bas                           |
| `→` / `Tab`    | Cycle vers le panel suivant                     |
| `←` / `BackTab`| Cycle vers le panel précédent                   |
| `Enter`        | Drill dans le panel suivant (Traces, Findings, Detail) |
| `Esc`          | Retour au panel précédent                       |
| `a`            | Acknowledger le finding sélectionné (mode live) |
| `u`            | Révoquer l'ack existant (mode live)             |

`a` et `u` sont no-op en mode batch (`inspect --input`) puisque
l'acknowledgment a besoin d'un daemon qui tourne pour persister.

## Flow d'acknowledgment (mode live)

Quand le TUI est lancé via `query inspect`, il fetch les findings avec
`?include_acked=true` pour que les findings déjà acknowledged
apparaissent dans la liste avec un indicateur italique gris
`[acked by <user>]` à droite de la ligne.

### `a` : créer un ack

Presser `a` sur un finding sélectionné ouvre une modale centrée sur
l'écran avec trois champs d'input :

| Champ   | Contrainte                                  | Default                |
|---------|---------------------------------------------|------------------------|
| Reason  | 1 à 256 chars, single-line                  | vide (requis)          |
| Expires | vide, `24h`, `7d`, ISO8601, etc             | vide (pas d'expiration)|
| By      | 1 à 128 chars                               | env var `$USER`        |

Plus deux boutons (`Submit` / `Cancel`).

Navigation modale :

| Touche           | Action                                       |
|------------------|----------------------------------------------|
| `Tab`            | Focus sur le champ ou bouton suivant         |
| `BackTab`        | Focus arrière                                |
| `Enter` (texte)  | Avance au champ suivant                      |
| `Enter` (Submit) | Envoie le formulaire                         |
| `Enter` (Cancel) | Ferme la modale sans envoyer                 |
| `Esc`            | Annule la modale                             |
| `Backspace`      | Supprime le dernier char du buffer focus     |

Au submit, le TUI poste sur `/api/findings/<sig>/ack` et ferme la
modale sur 201. En cas d'erreur (4xx/5xx), la modale reste ouverte
avec le message d'erreur en bas (texte rouge).

### `u` : révoquer un ack

Presser `u` sur un finding acknowledged ouvre une modale de
confirmation. `Submit` / `Enter` envoie un `DELETE
/api/findings/<sig>/ack`. `Cancel` / `Esc` ferme sans révoquer.

### Format expires

Mirror du CLI ack helper (depuis 0.5.22) :

- Vide : pas d'expiration, l'ack persiste jusqu'à revoke manuel
- `24h`, `7d`, `30m` : durée relative parsée par humantime
- `2026-05-11T00:00:00Z` : datetime ISO8601 absolu

Une entrée invalide affiche `expires: <erreur>` dans le footer de la
modale sans envoyer la requête.

## Authentification

Le TUI mirror la résolution d'auth du CLI ack helper :

1. Variable d'env `PERF_SENTINEL_DAEMON_API_KEY` (priorité 1)
2. Flag `--api-key-file <path>` sur `query inspect` (priorité 2)

```bash
# variable d'env
export PERF_SENTINEL_DAEMON_API_KEY=$(cat ~/.config/perf-sentinel/key)
perf-sentinel query --daemon http://localhost:4318 inspect

# fichier
perf-sentinel query --daemon http://localhost:4318 inspect \
  --api-key-file ~/.config/perf-sentinel/key
```

Les deux sont équivalents. Le path supporte le refus de symlink via
`O_NOFOLLOW` sur Unix et trim les newlines en fin de fichier.

**Pas de prompt password interactif dans le TUI.** Le raw mode et
l'alternate screen sont incompatibles avec l'input TTY de
`rpassword`. Si le daemon répond 401 sans clé env ou file, la modale
affiche "API key required: set PERF_SENTINEL_DAEMON_API_KEY or pass
--api-key-file when launching `query inspect`." Quitter, set la clé,
relancer.

Quand le daemon n'a pas de `[daemon.ack] api_key` configuré (default
pour les déploiements loopback), aucune clé n'est requise et la
modale envoie directement.

## Caveats

### Le HTTP synchrone freeze l'UI

`run_loop` est synchrone et le write daemon ack est exécuté via
`tokio::runtime::Handle::current().block_on(...)` depuis l'intérieur
de la loop. L'UI freeze pour la durée de la requête, typiquement
100-300 ms en localhost, plus long sur le réseau. Acceptable pour une
release scope-minimal. Un refactor en async event loop est un
followup candidat si le feedback utilisateur signale de la friction.

### Snapshot de la liste des findings

La liste des findings est fetchée une seule fois au boot. `a`/`u`
rafraîchissent uniquement l'état des acks via un second `GET
/api/findings?include_acked=true`, la liste des findings elle-même ne
change pas en cours de session. Pour récupérer des traces nouvellement
ingérées, quitter et relancer.

### Acks TOML visibles, pas modifiables

Les findings ack dans `.perf-sentinel-acknowledgments.toml` (CI ack)
apparaissent avec l'indicateur `[acked by <user>]` et le champ source
positionné à `toml`. Le TUI ne peut pas promouvoir un ack daemon vers
TOML ni éditer le fichier TOML. Pour les acks permanents, éditer le
fichier via revue PR comme décrit dans
[`ACK-WORKFLOW-FR.md`](./ACK-WORKFLOW-FR.md).

## Voir aussi

- [`ACK-WORKFLOW-FR.md`](./ACK-WORKFLOW-FR.md) pour la relation entre
  les acks TOML CI et les acks JSONL daemon, plus la table de
  décision complète.
- [`CLI-FR.md`](./CLI-FR.md) pour la référence de la sous-commande
  `perf-sentinel ack` (équivalent CLI de `a`/`u`).
- [`HTML-REPORT-FR.md`](./HTML-REPORT-FR.md) pour le flow ack
  navigateur via `--daemon-url`.
- [`CONFIGURATION-FR.md`](./CONFIGURATION-FR.md) pour la référence
  config `[daemon.ack]` côté serveur.
