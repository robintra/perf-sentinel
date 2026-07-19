# Comprendre les chiffres d'énergie et de carbone

Cette page raconte toute l'histoire de l'énergie en langage clair : ce que perf-sentinel compte, ce qu'il mesure, comment un nombre d'opérations d'I/O devient des kilowattheures et des grammes de CO2, et ce qui change selon les options activées. C'est une synthèse, pas une référence. Les formules vivent dans [METHODOLOGY-FR.md](METHODOLOGY-FR.md), les bornes de précision dans [LIMITATIONS-FR.md](LIMITATIONS-FR.md), et chaque clé de configuration dans [CONFIGURATION-FR.md](CONFIGURATION-FR.md).

## L'idée en un paragraphe

perf-sentinel lit des traces distribuées et compte chaque opération d'I/O qu'une application effectue : requêtes SQL et appels HTTP sortants. Ses détecteurs signalent les opérations qui n'avaient pas besoin d'exister, principalement les boucles N+1 et les appels répétés redondants. Le rapport entre opérations évitables et opérations totales est le ratio de gaspillage, et c'est le chiffre le plus robuste que produit l'outil parce qu'il ne dépend d'aucun modèle d'énergie. Tout le reste de cette page consiste à transformer les comptages en énergie et en carbone : le ratio de gaspillage dit quelle part est gaspillée, la chaîne d'énergie dit combien cette part pèse en kWh et en gCO2.

## D'où vient chaque chiffre

Les chiffres de carbone suivent le modèle Software Carbon Intensity (SCI, normalisé ISO/IEC 21031:2024) : carbone = énergie x intensité du réseau électrique, plus un terme incorporé pour le matériel lui-même.

- **L'énergie (E)** démarre comme une estimation et devient une mesure à mesure que vous branchez des backends. Sans rien configurer, chaque opération coûte un forfait de `1e-7 kWh` (le coefficient proxy, étiquette de modèle `io_proxy_v3`). Chaque backend ci-dessous remplace cette estimation par quelque chose de plus proche de la réalité physique.
- **L'intensité du réseau (I)** convertit les kWh en gCO2 pour la région où le code tourne. Elle part de moyennes nationales annuelles embarquées, peut être modulée par des profils sur 24 heures, et devient une valeur en direct quand l'API Electricity Maps est configurée. La région elle-même vient de l'attribut de span `cloud.region`, de `[green.service_regions]`, ou de `[green] default_region`, dans cet ordre.
- **Le carbone incorporé (M)** rend compte de la fabrication des serveurs. C'est un forfait par requête dérivé d'analyses de cycle de vie publiques de serveurs rack (Boavizta et la méthodologie Cloud Carbon Footprint), configurable via `embodied_carbon_per_request_gco2`. Corriger un N+1 ne dé-fabrique pas le silicium, donc les chiffres évitables n'incluent jamais ce terme.
- **Le PUE** multiplie l'énergie pour rendre compte des surcoûts du centre de données (refroidissement, distribution électrique) : 1,09 pour GCP, 1,15 pour AWS, 1,17 pour Azure, 1,5 pour une infrastructure inconnue.

Chaque rapport dit quel modèle a produit ses chiffres via les étiquettes `energy_model` et `per_service_energy_model`, un lecteur peut donc toujours distinguer une estimation d'une mesure.

## L'échelle de fidélité

Chaque ligne de ce tableau remplace ou affine la ligne au-dessus. Vous pouvez vous arrêter à n'importe quel barreau, les rapports restent honnêtes sur le barreau où vous êtes.

| Vous configurez                                | Ce que vous obtenez                                                                                                                                                                                                                         | Étiquette de modèle            |
|------------------------------------------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|--------------------------------|
| Rien (défaut)                                  | Un forfait de `1e-7 kWh` par opération, pondéré par verbe SQL (SELECT 0,5x, INSERT/UPDATE 1,5x, DELETE 1,2x) et par paliers de taille de réponse HTTP. Directionnel, en ordre de grandeur, avec un encadrement d'incertitude de 2x.         | `io_proxy_v3`                  |
| `calibrate` depuis un CSV de puissance mesurée | Le coefficient proxy est recalé par service depuis vos propres joules par opération mesurés. Toujours un modèle, mais ancré sur votre matériel.                                                                                             | `io_proxy_*+cal`               |
| `[green.cloud]`                                | L'énergie des VM cloud interpolée depuis l'utilisation CPU et la base publique SPECpower de courbes de puissance de serveurs, selon la méthodologie Cloud Carbon Footprint.                                                                 | `cloud_specpower`              |
| `[green.redfish]`                              | La puissance à la prise lue depuis le BMC du serveur. Le seul backend qui voit les ventilateurs, les disques et les pertes d'alimentation. Bare metal seulement.                                                                            | `redfish_bmc`                  |
| `[green.kepler]`                               | Les estimations par container de Kepler. Classé sous les backends RAPL parce qu'une évaluation indépendante a mesuré de grandes erreurs d'attribution, voir les sources.                                                                    | `kepler_ebpf`                  |
| `[green.scaphandre]`                           | L'énergie CPU depuis les compteurs Intel RAPL, attribuée par processus par Scaphandre.                                                                                                                                                      | `scaphandre_rapl`              |
| `[green.alumet]`                               | L'énergie CPU depuis RAPL, attribuée par cgroup par Alumet. Le backend mesuré recommandé : mêmes compteurs que Scaphandre, échantillonnage caractérisé comme moins sujet à erreur par ses auteurs, attribution taillée pour les containers. | `alumet_rapl`                  |
| `[green.electricity_maps]`                     | Ne change pas E. Remplace l'intensité annuelle par la valeur en direct de votre région, le plus gros levier sur les chiffres de gCO2 dans les régions au mix électrique variable.                                                           | source d'intensité `real_time` |

Quand plusieurs backends couvrent le même service, le daemon garde la lecture la plus fidèle : `alumet_rapl` bat `scaphandre_rapl`, qui bat `kepler_ebpf`, puis `redfish_bmc`, puis `cloud_specpower`, puis le proxy. Tous les backends mesurés sont réservés au daemon (`watch`), le mode batch `analyze` utilise toujours le chemin proxy.

Une contrainte honnête s'applique à tous les barreaux RAPL : les compteurs matériels ne voient que le CPU et la DRAM, soit environ la moitié aux deux tiers de ce que le serveur tire à la prise. Seul Redfish voit le reste.

## Le chiffre de la base de données

Compter les opérations côté application manque un point structurel : l'énergie d'un N+1 est surtout brûlée par la base de données qui exécute les N requêtes, et une base n'émet pas de spans, elle est donc invisible pour l'attribution par service. La déclaration `[green.alumet.database]` comble ce trou avec une règle de trois volontairement simple : pointez Alumet sur le cgroup de la base, et chaque fenêtre de scoring multiplie l'énergie mesurée de la base par le ratio de gaspillage SQL seul.

```
gaspillage base = énergie DB mesurée x (ops SQL évitables / ops SQL totales)
```

Le résultat est `green_summary.database_waste`, avec une conversion gCO2 quand vous déclarez la région de la base. Et le chiffre existe même sans Alumet : quand aucune mesure n'est disponible (exécutions batch, bases managées, pas de `[green.alumet.database]`), il est estimé depuis l'énergie modélisée des spans SQL, et son étiquette `model` dit quel chemin l'a produit (`alumet_rapl` = mesuré, `estimated` = modélisé). Le mesuré est une borne basse (énergie CPU seulement, ni DRAM ni disque), l'estimé hérite de l'encadrement 2x du proxy, les deux utilisent un ratio par comptage, le chiffre reste donc informatif : la variante mesurée est de l'énergie additionnelle exclue de `energy_kwh` et de `co2`, la variante estimée est une part re-présentée de ces totaux (ne jamais l'additionner par-dessus), et la divulgation ne le publie que comme bloc séparé étiqueté hors de tous les totaux. La liste complète des bornes est dans [LIMITATIONS-FR.md](LIMITATIONS-FR.md#limites-de-précision-alumet).

## Ce que les chiffres ne sont pas

L'outil est un compteur directionnel de gaspillage avec un ancrage énergétique de mieux en mieux mesuré, pas un wattmètre et pas un inventaire carbone certifié. Le chemin proxy porte un encadrement multiplicatif de 2x. La puissance idle et statique des serveurs n'est pas redistribuée aux services. Les ratios par comptage traitent pareil un SELECT indexé bon marché et une écriture lourde, alors que les mesures académiques montrent des écarts de puissance de plusieurs dizaines de pour cent. Tout cela est quantifié, avec le raisonnement, dans [LIMITATIONS-FR.md](LIMITATIONS-FR.md).

## Sources

Ce que chaque source externe apporte aux chiffres ci-dessus :

- **Green Software Foundation, spécification Software Carbon Intensity (ISO/IEC 21031:2024)** : le cadre `carbone = E x I + M`, l'exigence d'intensité location-based, et l'obligation de divulguer la méthodologie derrière chaque chiffre.
- **Tsirogiannis, Harizopoulos, Shah, "Analyzing the Energy Efficiency of a Database Server", SIGMOD 2010** : à utilisation CPU égale, des opérateurs de base de données peuvent différer de jusqu'à 60 % en puissance. Fonde les multiplicateurs par verbe SQL et la réserve d'honnêteté sur les ratios par comptage.
- **Xu, Tu, Wang, "Exploring Power-Performance Tradeoffs in Database Systems", ICDE 2010** et **Lella et al., "DBJoules: An Energy Measurement Tool for Database Management Systems", arXiv:2311.08961** : le coût énergétique relatif des classes d'opérations SQL derrière la pondération par verbe.
- **Khan et al., "RAPL in Action: Experiences in Using RAPL for Power Measurements", ACM TOMPECS 2018** : les lectures RAPL corrèlent étroitement avec des mesures externes à la prise, la raison pour laquelle les backends RAPL passent devant ceux à base de modèle.
- **Raffin, Trystram, "Dissecting the software-based measurement of CPU energy consumption: a comparative analysis", arXiv:2401.15985 (IEEE TPDS 2025)** : les pièges des lecteurs RAPL logiciels, et la raison pour laquelle `alumet_rapl` surclasse `scaphandre_rapl`.
- **Raffin, Trystram, Richard, "Alumet: a Modular Framework to Standardize the Measurement of Energy Consumption", PECS 2025** : le cadre de mesure derrière le backend recommandé.
- **Pijnacker et al., "Container-level Energy Observability in Kubernetes Clusters", arXiv:2504.10702** et le **billet CNCF "Kepler, re-architected" (juin 2026)** : des erreurs d'attribution mesurées indépendamment dans le modèle eBPF de Kepler et la refonte amont qui a suivi, la raison pour laquelle `kepler_ebpf` se place sous les backends RAPL.
- **Mytton, Lunden, Malmodin, "Network energy use not directly proportional to data volume", Journal of Industrial Ecology 28(4), 2024** : pourquoi le terme optionnel de transport réseau est modélisé prudemment.
- **Les résultats publiés SPEC SPECpower_ssj2008 et la méthodologie Cloud Carbon Footprint** : l'interpolation utilisation-vers-watts derrière `cloud_specpower`.
- **Boavizta** : les analyses de cycle de vie de serveurs derrière le terme incorporé.
- **Electricity Maps** : l'API d'intensité en direct derrière la source d'intensité `real_time`.

La justification de conception plus profonde, y compris la normalisation de la forme de lecture de chaque backend, vit dans `docs/FR/design/05-GREENOPS-AND-CARBON-FR.md`.
