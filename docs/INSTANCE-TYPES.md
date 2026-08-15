# Instance types with an embedded power profile

Every `instance_type` accepted by `[green.cloud.services]` and by
`[green.broker_static]`, with the idle and maximum wattage the SPECpower
model interpolates between. 356 entries, table vintage
`2026-04-24 (CCF aligned)`.

**An unlisted type is not an error.** perf-sentinel warns once at startup,
naming the type, and falls back to a provider-level average: the figure
gets coarser, nothing breaks. That warning is also how you check your own
type without reading this page. When your hardware is absent and you know
its draw, declare it directly instead, which is exact rather than
approximated:

```toml
[green.cloud.services]
"my-service" = { idle_watts = 45.0, max_watts = 120.0 }
```

**Only AWS, GCP and Azure families are listed**, and `[green.cloud]`
accepts only those three providers. The wattages come from the Cloud
Carbon Footprint coefficient CSVs, which publish nothing for OVHcloud,
Scaleway or OUTSCALE. Those three carry grid intensity and PUE in the
carbon table, so their regions score, but no published figure supports a
per-instance power profile for them, and a made-up wattage would not be
an estimate. Searched, as of August 2026:

- **Boavizta** ([BoaviztAPI](https://github.com/Boavizta/boaviztapi)) is
  the only third-party base that reaches instance granularity, with 50
  OVHcloud sizes and 69 Scaleway ones. Its files carry **no power
  column** at all, and for OVHcloud the CPU those wattages would be
  derived from is Boavizta's own assumption, flagged `CPU not verified`
  on all 12 archetype rows. One credits a rack server with an
  `Intel Core i7-4940MX`, a mobile part, across two sockets.
- **Scaleway** publishes only `kg_co2_equivalent` and `m3_water_usage`,
  account-scoped, and states that instead of measuring instance power it
  feeds a CPU-percentage proxy into a Boavizta consumption profile. Its
  Product Catalog API does expose CPU sockets, cores and frequency per
  SKU, which is a lead, but no wattage.
- **OUTSCALE** stops at (Region, service category): two regions, three
  categories, no instance type, no watt, no kWh.

On that hardware, measure instead: Alumet or
Scaphandre read RAPL directly and outrank every modeled figure.

Where the numbers come from, and why a family maps to a coefficient
rather than to a measured machine: [`METHODOLOGY.md`](./METHODOLOGY.md)
and `docs/design/05-GREENOPS-AND-CARBON.md`. Configuring the scraper:
[`CONFIGURATION.md`](./CONFIGURATION.md).

This page is generated from the embedded table by
`scripts/generate-instance-types-doc.py`, and a test fails the build if
the two ever disagree. Do not edit it by hand.

## AWS (184 entries)

| Instance type  | Idle (W) | Max (W) |
|----------------|---------:|--------:|
| `c5.12xlarge`  |     33.1 |     195 |
| `c5.18xlarge`  |     49.7 |   292.5 |
| `c5.24xlarge`  |     66.3 |   390.1 |
| `c5.2xlarge`   |      5.5 |    32.5 |
| `c5.4xlarge`   |       11 |      65 |
| `c5.9xlarge`   |     24.9 |   146.3 |
| `c5.large`     |      1.4 |     8.1 |
| `c5.xlarge`    |      2.8 |    16.3 |
| `c5a.12xlarge` |     22.8 |    81.3 |
| `c5a.16xlarge` |     30.4 |   108.3 |
| `c5a.24xlarge` |     45.5 |   162.5 |
| `c5a.2xlarge`  |      3.8 |    13.5 |
| `c5a.4xlarge`  |      7.6 |    27.1 |
| `c5a.8xlarge`  |     15.2 |    54.2 |
| `c5a.large`    |      0.9 |     3.4 |
| `c5a.xlarge`   |      1.9 |     6.8 |
| `c6a.16xlarge` |     29.2 |   125.3 |
| `c6a.2xlarge`  |      3.6 |    15.7 |
| `c6a.4xlarge`  |      7.3 |    31.3 |
| `c6a.8xlarge`  |     14.6 |    62.6 |
| `c6a.large`    |      0.9 |     3.9 |
| `c6a.xlarge`   |      1.8 |     7.8 |
| `c6i.12xlarge` |     36.8 |   180.4 |
| `c6i.16xlarge` |     49.1 |   240.5 |
| `c6i.24xlarge` |     73.6 |   360.8 |
| `c6i.2xlarge`  |      6.1 |    30.1 |
| `c6i.32xlarge` |     98.2 |     481 |
| `c6i.4xlarge`  |     12.3 |    60.1 |
| `c6i.8xlarge`  |     24.5 |   120.3 |
| `c6i.large`    |      1.5 |     7.5 |
| `c6i.xlarge`   |      3.1 |      15 |
| `c7a.16xlarge` |     47.3 |   146.1 |
| `c7a.2xlarge`  |      5.9 |    18.3 |
| `c7a.4xlarge`  |     11.8 |    36.5 |
| `c7a.8xlarge`  |     23.7 |      73 |
| `c7a.large`    |      1.5 |     4.6 |
| `c7a.xlarge`   |        3 |     9.1 |
| `c7g.16xlarge` |     30.4 |   108.3 |
| `c7g.2xlarge`  |      3.8 |    13.5 |
| `c7g.4xlarge`  |      7.6 |    27.1 |
| `c7g.8xlarge`  |     15.2 |    54.2 |
| `c7g.large`    |      0.9 |     3.4 |
| `c7g.xlarge`   |      1.9 |     6.8 |
| `c7i.16xlarge` |     66.3 |   266.3 |
| `c7i.2xlarge`  |      8.3 |    33.3 |
| `c7i.4xlarge`  |     16.6 |    66.6 |
| `c7i.8xlarge`  |     33.2 |   133.1 |
| `c7i.large`    |      2.1 |     8.3 |
| `c7i.xlarge`   |      4.1 |    16.6 |
| `c8a.16xlarge` |     47.3 |   146.1 |
| `c8a.2xlarge`  |      5.9 |    18.3 |
| `c8a.4xlarge`  |     11.8 |    36.5 |
| `c8a.8xlarge`  |     23.7 |      73 |
| `c8a.large`    |      1.5 |     4.6 |
| `c8a.xlarge`   |        3 |     9.1 |
| `c8g.16xlarge` |     30.4 |   108.3 |
| `c8g.2xlarge`  |      3.8 |    13.5 |
| `c8g.4xlarge`  |      7.6 |    27.1 |
| `c8g.8xlarge`  |     15.2 |    54.2 |
| `c8g.large`    |      0.9 |     3.4 |
| `c8g.xlarge`   |      1.9 |     6.8 |
| `c8i.16xlarge` |     52.1 |   286.9 |
| `c8i.2xlarge`  |      6.5 |    35.9 |
| `c8i.4xlarge`  |       13 |    71.7 |
| `c8i.8xlarge`  |       26 |   143.4 |
| `c8i.large`    |      1.6 |       9 |
| `c8i.xlarge`   |      3.3 |    17.9 |
| `m5.12xlarge`  |     33.1 |     195 |
| `m5.16xlarge`  |     44.2 |     260 |
| `m5.24xlarge`  |     66.3 |   390.1 |
| `m5.2xlarge`   |      5.5 |    32.5 |
| `m5.4xlarge`   |       11 |      65 |
| `m5.8xlarge`   |     22.1 |     130 |
| `m5.large`     |      1.4 |     8.1 |
| `m5.xlarge`    |      2.8 |    16.3 |
| `m5a.12xlarge` |     40.6 |     125 |
| `m5a.16xlarge` |     54.2 |   166.7 |
| `m5a.24xlarge` |     81.3 |     250 |
| `m5a.2xlarge`  |      6.8 |    20.8 |
| `m5a.4xlarge`  |     13.5 |    41.7 |
| `m5a.8xlarge`  |     27.1 |    83.3 |
| `m5a.large`    |      1.7 |     5.2 |
| `m5a.xlarge`   |      3.4 |    10.4 |
| `m6a.16xlarge` |     29.2 |   125.3 |
| `m6a.2xlarge`  |      3.6 |    15.7 |
| `m6a.4xlarge`  |      7.3 |    31.3 |
| `m6a.8xlarge`  |     14.6 |    62.6 |
| `m6a.large`    |      0.9 |     3.9 |
| `m6a.xlarge`   |      1.8 |     7.8 |
| `m6i.12xlarge` |     36.8 |   180.4 |
| `m6i.16xlarge` |     49.1 |   240.5 |
| `m6i.24xlarge` |     73.6 |   360.8 |
| `m6i.2xlarge`  |      6.1 |    30.1 |
| `m6i.32xlarge` |     98.2 |     481 |
| `m6i.4xlarge`  |     12.3 |    60.1 |
| `m6i.8xlarge`  |     24.5 |   120.3 |
| `m6i.large`    |      1.5 |     7.5 |
| `m6i.xlarge`   |      3.1 |      15 |
| `m7a.16xlarge` |     47.3 |   146.1 |
| `m7a.2xlarge`  |      5.9 |    18.3 |
| `m7a.4xlarge`  |     11.8 |    36.5 |
| `m7a.8xlarge`  |     23.7 |      73 |
| `m7a.large`    |      1.5 |     4.6 |
| `m7a.xlarge`   |        3 |     9.1 |
| `m7g.16xlarge` |     30.4 |   108.3 |
| `m7g.2xlarge`  |      3.8 |    13.5 |
| `m7g.4xlarge`  |      7.6 |    27.1 |
| `m7g.8xlarge`  |     15.2 |    54.2 |
| `m7g.large`    |      0.9 |     3.4 |
| `m7g.xlarge`   |      1.9 |     6.8 |
| `m7i.16xlarge` |     66.3 |   266.3 |
| `m7i.2xlarge`  |      8.3 |    33.3 |
| `m7i.4xlarge`  |     16.6 |    66.6 |
| `m7i.8xlarge`  |     33.2 |   133.1 |
| `m7i.large`    |      2.1 |     8.3 |
| `m7i.xlarge`   |      4.1 |    16.6 |
| `m8a.16xlarge` |     47.3 |   146.1 |
| `m8a.2xlarge`  |      5.9 |    18.3 |
| `m8a.4xlarge`  |     11.8 |    36.5 |
| `m8a.8xlarge`  |     23.7 |      73 |
| `m8a.large`    |      1.5 |     4.6 |
| `m8a.xlarge`   |        3 |     9.1 |
| `m8g.16xlarge` |     30.4 |   108.3 |
| `m8g.2xlarge`  |      3.8 |    13.5 |
| `m8g.4xlarge`  |      7.6 |    27.1 |
| `m8g.8xlarge`  |     15.2 |    54.2 |
| `m8g.large`    |      0.9 |     3.4 |
| `m8g.xlarge`   |      1.9 |     6.8 |
| `m8i.16xlarge` |     52.1 |   286.9 |
| `m8i.2xlarge`  |      6.5 |    35.9 |
| `m8i.4xlarge`  |       13 |    71.7 |
| `m8i.8xlarge`  |       26 |   143.4 |
| `m8i.large`    |      1.6 |       9 |
| `m8i.xlarge`   |      3.3 |    17.9 |
| `r5.12xlarge`  |     40.8 |   214.2 |
| `r5.16xlarge`  |     54.4 |   285.6 |
| `r5.24xlarge`  |     81.6 |   428.5 |
| `r5.2xlarge`   |      6.8 |    35.7 |
| `r5.4xlarge`   |     13.6 |    71.4 |
| `r5.8xlarge`   |     27.2 |   142.8 |
| `r5.large`     |      1.7 |     8.9 |
| `r5.xlarge`    |      3.4 |    17.9 |
| `r5a.12xlarge` |     48.3 |   144.2 |
| `r5a.16xlarge` |     64.4 |   192.3 |
| `r5a.24xlarge` |     96.7 |   288.4 |
| `r5a.2xlarge`  |      8.1 |      24 |
| `r5a.4xlarge`  |     16.1 |    48.1 |
| `r5a.8xlarge`  |     32.2 |    96.1 |
| `r5a.large`    |        2 |       6 |
| `r5a.xlarge`   |        4 |      12 |
| `r6i.12xlarge` |     44.5 |   199.6 |
| `r6i.16xlarge` |     59.3 |   266.1 |
| `r6i.24xlarge` |       89 |   399.2 |
| `r6i.2xlarge`  |      7.4 |    33.3 |
| `r6i.4xlarge`  |     14.8 |    66.5 |
| `r6i.8xlarge`  |     29.7 |   133.1 |
| `r6i.large`    |      1.9 |     8.3 |
| `r6i.xlarge`   |      3.7 |    16.6 |
| `r7a.16xlarge` |     57.5 |   171.7 |
| `r7a.2xlarge`  |      7.2 |    21.5 |
| `r7a.4xlarge`  |     14.4 |    42.9 |
| `r7a.8xlarge`  |     28.8 |    85.8 |
| `r7a.large`    |      1.8 |     5.4 |
| `r7a.xlarge`   |      3.6 |    10.7 |
| `r7i.16xlarge` |     76.6 |   291.9 |
| `r7i.2xlarge`  |      9.6 |    36.5 |
| `r7i.4xlarge`  |     19.1 |      73 |
| `r7i.8xlarge`  |     38.3 |   145.9 |
| `r7i.large`    |      2.4 |     9.1 |
| `r7i.xlarge`   |      4.8 |    18.2 |
| `t3.2xlarge`   |      5.5 |    32.5 |
| `t3.large`     |      1.4 |     8.1 |
| `t3.medium`    |      1.4 |     8.1 |
| `t3.micro`     |      1.4 |     8.1 |
| `t3.nano`      |      1.4 |     8.1 |
| `t3.small`     |      1.4 |     8.1 |
| `t3.xlarge`    |      2.8 |    16.3 |
| `t3a.2xlarge`  |      6.8 |    20.8 |
| `t3a.large`    |      1.7 |     5.2 |
| `t3a.medium`   |      1.7 |     5.2 |
| `t3a.micro`    |      1.7 |     5.2 |
| `t3a.nano`     |      1.7 |     5.2 |
| `t3a.small`    |      1.7 |     5.2 |
| `t3a.xlarge`   |      3.4 |    10.4 |

## GCP (83 entries)

| Instance type      | Idle (W) | Max (W) |
|--------------------|---------:|--------:|
| `c2-standard-16`   |       11 |    60.1 |
| `c2-standard-30`   |     20.7 |   112.6 |
| `c2-standard-4`    |      2.8 |      15 |
| `c2-standard-60`   |     41.4 |   225.3 |
| `c2-standard-8`    |      5.5 |      30 |
| `c3-standard-176`  |    182.4 |   714.9 |
| `c3-standard-22`   |     22.8 |    89.4 |
| `c3-standard-4`    |      4.1 |    16.2 |
| `c3-standard-44`   |     45.6 |   178.7 |
| `c3-standard-8`    |      8.3 |    32.5 |
| `c3-standard-88`   |     91.2 |   357.5 |
| `c3d-standard-16`  |     11.8 |    35.1 |
| `c3d-standard-180` |      133 |   395.3 |
| `c3d-standard-30`  |     22.2 |    65.9 |
| `c3d-standard-4`   |        3 |     8.8 |
| `c3d-standard-60`  |     44.3 |   131.8 |
| `c3d-standard-8`   |      5.9 |    17.6 |
| `c4-standard-16`   |       13 |    70.1 |
| `c4-standard-2`    |      1.6 |     8.8 |
| `c4-standard-32`   |       26 |   140.2 |
| `c4-standard-4`    |      3.3 |    17.5 |
| `c4-standard-8`    |      6.5 |    35.1 |
| `c4-standard-96`   |     78.1 |   420.6 |
| `c4a-standard-1`   |      0.5 |     1.7 |
| `c4a-standard-16`  |      7.6 |    27.1 |
| `c4a-standard-2`   |      0.9 |     3.4 |
| `c4a-standard-32`  |     15.2 |    54.2 |
| `c4a-standard-4`   |      1.9 |     6.8 |
| `c4a-standard-48`  |     22.8 |    81.3 |
| `c4a-standard-72`  |     34.1 |   121.9 |
| `c4a-standard-8`   |      3.8 |    13.5 |
| `c4d-standard-16`  |      5.1 |    30.6 |
| `c4d-standard-2`   |      0.6 |     3.8 |
| `c4d-standard-32`  |     10.2 |    61.1 |
| `c4d-standard-4`   |      1.3 |     7.6 |
| `c4d-standard-8`   |      2.6 |    15.3 |
| `c4d-standard-96`  |     30.7 |   183.4 |
| `e2-standard-16`   |      7.6 |    25.2 |
| `e2-standard-2`    |      0.9 |     3.2 |
| `e2-standard-32`   |     15.2 |    50.4 |
| `e2-standard-4`    |      1.9 |     6.3 |
| `e2-standard-8`    |      3.8 |    12.6 |
| `n2-highcpu-16`    |       11 |    60.1 |
| `n2-highcpu-2`     |      1.4 |     7.5 |
| `n2-highcpu-32`    |     22.1 |   120.2 |
| `n2-highcpu-4`     |      2.8 |      15 |
| `n2-highcpu-48`    |     33.1 |   180.2 |
| `n2-highcpu-64`    |     44.2 |   240.3 |
| `n2-highcpu-8`     |      5.5 |      30 |
| `n2-highcpu-80`    |     55.2 |   300.4 |
| `n2-highcpu-96`    |     66.3 |   360.5 |
| `n2-highmem-128`   |    108.8 |   531.8 |
| `n2-highmem-16`    |     13.6 |    66.5 |
| `n2-highmem-2`     |      1.7 |     8.3 |
| `n2-highmem-32`    |     27.2 |     133 |
| `n2-highmem-4`     |      3.4 |    16.6 |
| `n2-highmem-48`    |     40.8 |   199.4 |
| `n2-highmem-64`    |     54.4 |   265.9 |
| `n2-highmem-8`     |      6.8 |    33.2 |
| `n2-highmem-80`    |       68 |   332.4 |
| `n2-highmem-96`    |     81.6 |   398.9 |
| `n2-standard-128`  |     88.4 |   480.6 |
| `n2-standard-16`   |       11 |    60.1 |
| `n2-standard-2`    |      1.4 |     7.5 |
| `n2-standard-32`   |     22.1 |   120.2 |
| `n2-standard-4`    |      2.8 |      15 |
| `n2-standard-48`   |     33.1 |   180.2 |
| `n2-standard-64`   |     44.2 |   240.3 |
| `n2-standard-8`    |      5.5 |      30 |
| `n2-standard-80`   |     55.2 |   300.4 |
| `n2-standard-96`   |     66.3 |   360.5 |
| `n2d-standard-16`  |     11.8 |    35.1 |
| `n2d-standard-2`   |      1.5 |     4.4 |
| `n2d-standard-32`  |     23.7 |    70.3 |
| `n2d-standard-4`   |        3 |     8.8 |
| `n2d-standard-64`  |     47.3 |   140.5 |
| `n2d-standard-8`   |      5.9 |    17.6 |
| `t2a-standard-1`   |      0.7 |     1.8 |
| `t2a-standard-16`  |     10.7 |      28 |
| `t2a-standard-2`   |      1.3 |     3.5 |
| `t2a-standard-32`  |     21.4 |      56 |
| `t2a-standard-4`   |      2.7 |       7 |
| `t2a-standard-8`   |      5.4 |      14 |

## Azure (88 entries)

| Instance type        | Idle (W) | Max (W) |
|----------------------|---------:|--------:|
| `Standard_D16ads_v6` |      6.4 |    32.8 |
| `Standard_D16as_v5`  |      7.1 |    32.3 |
| `Standard_D16ps_v6`  |      9.6 |    35.2 |
| `Standard_D16s_v3`   |     10.3 |    67.1 |
| `Standard_D16s_v4`   |     10.2 |    63.5 |
| `Standard_D16s_v5`   |     10.2 |    63.5 |
| `Standard_D16s_v6`   |      8.8 |    51.2 |
| `Standard_D2ads_v6`  |      0.8 |     4.1 |
| `Standard_D2as_v5`   |      0.9 |       4 |
| `Standard_D2ps_v6`   |      1.2 |     4.4 |
| `Standard_D2s_v3`    |      1.3 |     8.4 |
| `Standard_D2s_v4`    |      1.3 |     7.9 |
| `Standard_D2s_v5`    |      1.3 |     7.9 |
| `Standard_D2s_v6`    |      1.1 |     6.4 |
| `Standard_D32ads_v6` |     12.8 |    65.6 |
| `Standard_D32as_v5`  |     14.3 |    64.6 |
| `Standard_D32ps_v6`  |     19.2 |    70.4 |
| `Standard_D32s_v3`   |     20.6 |   134.2 |
| `Standard_D32s_v4`   |     20.4 |     127 |
| `Standard_D32s_v5`   |     20.4 |     127 |
| `Standard_D32s_v6`   |     17.6 |   102.4 |
| `Standard_D48as_v5`  |     21.4 |    96.9 |
| `Standard_D48s_v3`   |     30.9 |   201.3 |
| `Standard_D48s_v4`   |     30.7 |   190.4 |
| `Standard_D48s_v5`   |     30.7 |   190.4 |
| `Standard_D4ads_v6`  |      1.6 |     8.2 |
| `Standard_D4as_v5`   |      1.8 |     8.1 |
| `Standard_D4ps_v6`   |      2.4 |     8.8 |
| `Standard_D4s_v3`    |      2.6 |    16.8 |
| `Standard_D4s_v4`    |      2.6 |    15.9 |
| `Standard_D4s_v5`    |      2.6 |    15.9 |
| `Standard_D4s_v6`    |      2.2 |    12.8 |
| `Standard_D64ads_v6` |     25.6 |   131.2 |
| `Standard_D64as_v5`  |     28.5 |   129.2 |
| `Standard_D64ps_v6`  |     38.4 |   140.8 |
| `Standard_D64s_v3`   |     41.3 |   268.4 |
| `Standard_D64s_v4`   |     40.9 |   253.9 |
| `Standard_D64s_v5`   |     40.9 |   253.9 |
| `Standard_D64s_v6`   |     35.2 |   204.8 |
| `Standard_D8ads_v6`  |      3.2 |    16.4 |
| `Standard_D8as_v5`   |      3.6 |    16.2 |
| `Standard_D8ps_v6`   |      4.8 |    17.6 |
| `Standard_D8s_v3`    |      5.2 |    33.5 |
| `Standard_D8s_v4`    |      5.1 |    31.7 |
| `Standard_D8s_v5`    |      5.1 |    31.7 |
| `Standard_D8s_v6`    |      4.4 |    25.6 |
| `Standard_D96ads_v6` |     38.4 |   196.8 |
| `Standard_D96as_v5`  |     42.8 |   193.9 |
| `Standard_D96ps_v6`  |     57.6 |   211.2 |
| `Standard_D96s_v5`   |     61.3 |   380.9 |
| `Standard_D96s_v6`   |     52.8 |   307.2 |
| `Standard_E16s_v3`   |     12.9 |    73.5 |
| `Standard_E16s_v4`   |     12.8 |    69.9 |
| `Standard_E16s_v5`   |     12.8 |    69.9 |
| `Standard_E16s_v6`   |     11.4 |    57.6 |
| `Standard_E2s_v3`    |      1.6 |     9.2 |
| `Standard_E2s_v4`    |      1.6 |     8.7 |
| `Standard_E2s_v5`    |      1.6 |     8.7 |
| `Standard_E2s_v6`    |      1.4 |     7.2 |
| `Standard_E32s_v3`   |     25.7 |     147 |
| `Standard_E32s_v4`   |     25.6 |   139.8 |
| `Standard_E32s_v5`   |     25.6 |   139.8 |
| `Standard_E32s_v6`   |     22.7 |   115.2 |
| `Standard_E48s_v3`   |     38.6 |   220.5 |
| `Standard_E48s_v4`   |     38.3 |   209.6 |
| `Standard_E48s_v5`   |     38.3 |   209.6 |
| `Standard_E4s_v3`    |      3.2 |    18.4 |
| `Standard_E4s_v4`    |      3.2 |    17.5 |
| `Standard_E4s_v5`    |      3.2 |    17.5 |
| `Standard_E4s_v6`    |      2.8 |    14.4 |
| `Standard_E64s_v3`   |     51.5 |     294 |
| `Standard_E64s_v4`   |     51.1 |   279.5 |
| `Standard_E64s_v5`   |     51.1 |   279.5 |
| `Standard_E64s_v6`   |     45.4 |   230.4 |
| `Standard_E8s_v3`    |      6.4 |    36.7 |
| `Standard_E8s_v4`    |      6.4 |    34.9 |
| `Standard_E8s_v5`    |      6.4 |    34.9 |
| `Standard_E8s_v6`    |      5.7 |    28.8 |
| `Standard_E96s_v5`   |     76.7 |   419.3 |
| `Standard_E96s_v6`   |     68.2 |   345.6 |
| `Standard_F16s_v2`   |     10.2 |    63.5 |
| `Standard_F2s_v2`    |      1.3 |     7.9 |
| `Standard_F32s_v2`   |     20.4 |     127 |
| `Standard_F48s_v2`   |     30.7 |   190.4 |
| `Standard_F4s_v2`    |      2.6 |    15.9 |
| `Standard_F64s_v2`   |     40.9 |   253.9 |
| `Standard_F72s_v2`   |       46 |   285.6 |
| `Standard_F8s_v2`    |      5.1 |    31.7 |

## Bare metal (1 entry)

| Instance type | Idle (W) | Max (W) |
|---------------|---------:|--------:|
| `xeon-6780e`  |      100 |     420 |
