# Signature Sigstore et attestation SLSA

Ce document décrit les primitives cryptographiques ajoutées au schéma
`perf-sentinel-report/v1.0` à partir de v0.7.0. Le but est de
permettre à un consommateur de vérifier de bout en bout une
divulgation périodique publiée sans avoir à faire confiance à
perf-sentinel ni à l'organisation publiante au-delà de ce qui est
ancré dans l'infrastructure publique Sigstore.

## Pourquoi deux couches

Les rapports perf-sentinel reposent sur deux signatures
complémentaires :

- **Signature Sigstore** sur le rapport, ancrée dans le journal de
  transparence Rekor. Prouve que le rapport a été signé par une
  identité autorisée par l'organisation publiante et n'a pas été
  modifié depuis.
- **Provenance SLSA** sur le binaire perf-sentinel, produite par le
  workflow GitHub Actions de release du projet. Prouve que le
  binaire ayant calculé le rapport a été construit depuis le code
  source officiel par un builder reconnu, pas par un build personnel
  ou trafiqué.

Un consommateur qui vérifie les deux obtient une chaîne de
confiance complète :

```
code source -> attestation SLSA -> binaire -> rapport -> signature Sigstore
```

Les deux couches sont indépendantes : un opérateur peut signer un
rapport produit par un binaire non officiel (la signature prouve
toujours la paternité et l'intégrité, l'attestation binaire est
simplement absente). Ou un binaire officiel peut produire un
rapport jamais signé (`hash-only`). Le schéma rend les deux états
explicites via `integrity.integrity_level` :

| niveau                      | content_hash | signature | binary_attestation |
|-----------------------------|--------------|-----------|--------------------|
| `none`                      | absent       | absent    | absent             |
| `hash-only`                 | présent      | absent    | absent             |
| `signed`                    | présent      | présent   | absent             |
| `signed-with-attestation`   | présent      | présent   | présent            |
| `audited` (réservé)         | n/a          | n/a       | n/a                |

## Le flow d'attestation

Pour une divulgation `intent = "official"`, le workflow opérateur
est :

1. **Scoring** : le daemon écrit les archives par fenêtre en NDJSON
   sur la période (aucune implication signature).
2. **Disclose** : `perf-sentinel disclose --intent official ...
   --output report.json --emit-attestation attestation.intoto.jsonl`
   produit deux fichiers. Le `integrity.content_hash` du rapport
   reçoit le SHA-256 canonique. L'attestation est un statement
   in-toto v1 dont le `subject.digest.sha256` pin le SHA-256 du
   fichier rapport sur disque (pas le hash canonique, qui blank un
   champ).
3. **Signer** : l'opérateur lance `cosign attest --type custom
   --predicate attestation.intoto.jsonl --bundle bundle.sig
   report.json` contre Sigstore public. La signature est uploadée
   automatiquement dans Rekor (le projet refuse les bundles sans
   preuve d'inclusion Rekor au moment de la vérification).
4. **Mettre à jour le locator signature du rapport** : l'opérateur
   édite `report.json` pour ajouter `integrity.signature` avec
   les métadonnées qui permettent aux vérifieurs de localiser le
   bundle et l'entrée Rekor, puis bump
   `integrity_level` de `hash-only` à `signed` ou
   `signed-with-attestation`. Cette étape est manuelle aujourd'hui,
   une future subcommand `perf-sentinel sign` pourrait l'automatiser.
5. **Publier** : les trois fichiers (`report.json`,
   `attestation.intoto.jsonl`, `bundle.sig`) sont publiés à l'URL
   de transparence de l'opérateur.

Un consommateur télécharge les trois fichiers et lance
`perf-sentinel verify-hash --report report.json --attestation
attestation.intoto.jsonl --bundle bundle.sig` ou, plus court,
`perf-sentinel verify-hash --url https://example.fr/report.json`
qui fetch les sidecars par convention.

## Format statement in-toto v1

L'attestation produite par `disclose --emit-attestation` est un
document in-toto v1 à statement unique. Forme :

```json
{
  "_type": "https://in-toto.io/Statement/v1",
  "predicateType": "https://perf-sentinel.io/attestation/v1",
  "subject": [
    {
      "name": "report.json",
      "digest": { "sha256": "<64-hex>" }
    }
  ],
  "predicate": {
    "perf_sentinel_version": "0.7.0",
    "report_uuid": "...",
    "period": { "from_date": "2026-01-01", "to_date": "2026-03-31" },
    "intent": "official",
    "confidentiality_level": "public",
    "organisation": {
      "name": "Example SAS",
      "country": "FR",
      "identifiers": { "siren": "...", "domain": "..." }
    },
    "methodology_summary": {
      "sci_specification": "ISO/IEC 21031:2024",
      "conformance": "core-required",
      "calibration_applied": true,
      "period_coverage": 0.91
    }
  }
}
```

`predicateType` utilise le namespace `perf-sentinel.io` par
convention. Le domaine n'est pas formellement possédé par le
projet aujourd'hui, c'est la pratique standard pour les predicates
in-toto custom. Les vérifieurs identifient le predicate par
correspondance string exacte.

Le `subject.digest.sha256` est le SHA-256 du fichier rapport tel
qu'écrit sur disque, pas le champ `content_hash` canonique. Les
deux servent des buts différents : le hash canonique est
déterministe (clés triées, un champ blanké) et vit dans le
document. Le subject digest est le hash byte-level réel du fichier
et vit dans l'attestation.

## Commande cosign

Pour la signature Sigstore publique en keyless OIDC, la commande
recommandée pour les opérateurs est :

```bash
cosign attest \
    --type custom \
    --predicate attestation.intoto.jsonl \
    --bundle bundle.sig \
    report.json
```

L'issuer OIDC (flow navigateur ou token GitHub Actions) enregistre
l'identité du signataire dans le bundle. Les opérateurs qui
utilisent une instance Rekor privée passent
`--rekor-url https://rekor.internal.example.fr` qui matche leur
config `[reporting.sigstore].rekor_url`.

Le flag `--no-tlog-upload` est délibérément non supporté par
verify-hash : un bundle sans preuve d'inclusion Rekor est refusé
avec un message d'erreur clair. L'auditabilité publique est une
propriété du format, pas un opt-in optionnel.

## Flow de vérification

`perf-sentinel verify-hash` chaîne jusqu'à trois vérifications :

1. **Content hash** (Rust pur, toujours lancé). Recompute le
   SHA-256 canonique du rapport et compare à
   `integrity.content_hash`.
2. **Signature** (déléguée à `cosign verify-attestation`). Lancée
   quand `integrity.signature` est présent dans le rapport et que
   l'opérateur passe `--attestation` et `--bundle` (ou que le mode
   `--url` les fetch automatiquement).
3. **Attestation binaire** (déléguée à `slsa-verifier
   verify-artifact`). Aujourd'hui la sortie verify-hash imprime un
   résumé métadonnée et la commande `slsa-verifier` exacte à
   lancer contre le binaire téléchargé depuis
   `integrity.binary_verification_url`. Le fetch binaire + verify
   en une seule commande est un travail futur.

Codes de sortie : `0` trusted, `1` untrusted (un check a échoué),
`2` erreur fichier, `3` erreur réseau.

## Privacy sur Rekor public

Chaque signature uploadée dans Rekor Sigstore public produit une
entrée permanente, lisible par tous dans le journal de
transparence. L'entrée contient :

- L'identité signataire enregistrée par l'issuer OIDC (par exemple
  un email Google, une URL de workflow GitHub Actions avec
  org/repo).
- Le hash du payload signé (le statement in-toto ici).
- Un timestamp.

L'entrée ne contient ni le rapport lui-même ni son contenu. Les
opérateurs concernés par la fuite d'identité signataire peuvent
considérer :

- Utiliser un email service-account dédié pour la signature.
- Faire tourner une instance Rekor privée
  (`[reporting.sigstore].rekor_url`).
- Signer avec un workflow GitHub Actions dont l'URL d'identité est
  pré-divulguée par l'organisation.

Pour la plupart des usages transparence publique, faire fuiter
l'identité signataire est le résultat voulu : le consommateur
veut savoir quelle identité vouche pour le rapport.

## Modes d'échec

Ce qu'un consommateur doit conclure quand chaque check échoue :

- **Content hash FAIL** : le fichier est corrompu ou a été
  trafiqué après publication. Untrusted.
- **Signature FAIL** avec content_hash valide : le rapport
  lui-même est intact mais n'a plus de preuve Sigstore valide.
  Probablement le bundle a été remplacé, l'entrée Rekor a été
  révoquée, ou l'identité certificat ne matche pas le signataire
  revendiqué. Untrusted.
- **Signature SKIP** parce que `cosign` n'est pas installé :
  installer cosign et réessayer. Le rapport n'est pas
  nécessairement untrusted mais ne peut pas être vérifié dans
  l'install courant de l'utilisateur. Content hash seul est une
  garantie plus faible.
- **Binary attestation NotProvided** : le rapport a été produit
  par un binaire qui ne porte pas de métadonnées de provenance
  SLSA (par exemple un build de développement local). Content
  hash + signature Sigstore tiennent toujours, mais le
  consommateur ne peut pas vérifier ce qui a produit le rapport.
- **Binary attestation FAIL** : le binaire référencé par
  `integrity.binary_verification_url` ne matche pas l'attestation
  SLSA, ou le source-uri ne matche pas
  `github.com/robintra/perf-sentinel`. Traiter comme untrusted.

Le verdict global apparaît comme `TRUSTED` (content hash +
signature OK), `PARTIAL` (content hash OK mais signature
NotProvided ou Skip), ou `UNTRUSTED` (un FAIL).

## Renvois

- `docs/FR/SCHEMA-FR.md` documente la forme wire de
  `integrity.signature` et `integrity.binary_attestation`.
- `docs/FR/REPORTING-FR.md` est le workflow signature côté
  opérateur.
- `docs/FR/SUPPLY-CHAIN-FR.md` couvre l'intégration du générateur
  SLSA dans le workflow GitHub Actions de release.
- `docs/schemas/perf-sentinel-report-v1.json` porte les
  définitions JSON Schema autoritaires.
