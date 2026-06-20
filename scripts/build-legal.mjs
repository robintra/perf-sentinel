// Generates the standalone legal pages (mentions-legales.html, confidentialite.html).
// Lifts the vitrine <style> verbatim so the chrome (theme vars, fonts, footer, hovers)
// stays identical without duplicating CSS. FR only for now (French-law pages).
import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const SITE = join(dirname(fileURLToPath(import.meta.url)), '..');
const STYLE = readFileSync(join(SITE, 'index.html'), 'utf8').match(/<style>[\s\S]*?<\/style>/)[0];

const SUN = '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="var(--accent)" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="4.2"></circle><path d="M12 2.6v2.1M12 19.3v2.1M4.5 4.5l1.5 1.5M18 18l1.5 1.5M2.6 12h2.1M19.3 12h2.1M4.5 19.5l1.5-1.5M18 6l1.5-1.5"></path></svg>';
const MOON = '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="var(--accent)" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 12.8A9 9 0 1 1 11.2 3 7 7 0 0 0 21 12.8z"></path></svg>';
const GH = '<svg viewBox="0 0 24 24" width="17" height="17" fill="currentColor" aria-hidden="true"><path d="M12 .297c-6.63 0-12 5.373-12 12 0 5.303 3.438 9.8 8.205 11.385.6.113.82-.258.82-.577 0-.285-.01-1.04-.015-2.04-3.338.724-4.042-1.61-4.042-1.61C4.422 18.07 3.633 17.7 3.633 17.7c-1.087-.744.084-.729.084-.729 1.205.084 1.838 1.236 1.838 1.236 1.07 1.835 2.809 1.305 3.495.998.108-.776.417-1.305.76-1.605-2.665-.3-5.466-1.332-5.466-5.93 0-1.31.465-2.38 1.235-3.22-.135-.303-.54-1.523.105-3.176 0 0 1.005-.322 3.3 1.23.96-.267 1.98-.399 3-.405 1.02.006 2.04.138 3 .405 2.28-1.552 3.285-1.23 3.285-1.23.645 1.653.24 2.873.12 3.176.765.84 1.23 1.91 1.23 3.22 0 4.61-2.805 5.625-5.475 5.92.42.36.81 1.096.81 2.22 0 1.606-.015 2.896-.015 3.286 0 .315.21.69.825.57C20.565 22.092 24 17.592 24 12.297c0-6.627-5.373-12-12-12"></path></svg>';

const logo = '<img data-logo="light" src="/assets/logo-h-light.svg" alt="perf sentinel" style="height:38px;width:auto"><img data-logo="dark" src="/assets/logo-h-dark.svg" alt="perf sentinel" style="height:38px;width:auto">';

const header = `<header style="position:sticky;top:0;z-index:50;background:color-mix(in srgb,var(--bg) 85%,transparent);backdrop-filter:blur(10px);border-bottom:1px solid var(--border)"><div style="max-width:1120px;margin:0 auto;padding:14px 28px;display:flex;align-items:center;gap:18px">` +
  `<a href="/" style="display:flex;align-items:center;flex:none">${logo}</a>` +
  `<a href="/" style="font-size:14px;color:var(--text-2);text-decoration:none">&larr; Accueil</a>` +
  `<div style="margin-left:auto;display:flex;align-items:center;gap:10px">` +
  `<button id="themeBtn" aria-label="Theme" style="display:flex;align-items:center;gap:7px;font-family:'JetBrains Mono',monospace;font-size:12px;letter-spacing:.02em;color:var(--text-2);background:var(--surface);border:1px solid var(--border);border-radius:999px;padding:7px 13px;cursor:pointer"><span class="th-ico th-sun">${SUN}</span><span class="th-ico th-moon">${MOON}</span><span id="themeLbl">Clair</span></button>` +
  `<a data-plain href="https://github.com/robintra/perf-sentinel" aria-label="perf-sentinel on GitHub" style="display:flex;align-items:center;gap:8px;font-size:14px;font-weight:600;color:#FFFFFF;background:#24292F;border:1px solid rgba(240,246,252,.18);border-radius:8px;padding:8px 15px">${GH}GitHub</a>` +
  `</div></div></header>`;

const col = (title, links) =>
  `<div><div style="font-size:13px;font-weight:600;color:var(--text);margin-bottom:13px">${title}</div><div style="display:flex;flex-direction:column;gap:9px;font-size:14px;color:var(--text-2)">${links.map(([h, t]) => `<a href="${h}">${t}</a>`).join('')}</div></div>`;

const footer = `<footer style="border-top:1px solid var(--border);background:var(--surface-2)"><div style="max-width:1120px;margin:0 auto;padding:48px 28px 36px">` +
  `<div data-grid="ftcols" style="display:grid;grid-template-columns:1.5fr 1fr 1fr 1fr;gap:32px">` +
  `<div>${logo}<p style="margin:16px 0 0;font-size:13.5px;color:var(--text-2);max-width:240px;line-height:1.55">Détecteur d'anti-patterns d'I/O au niveau protocole, auto-hébergeable et carbon-aware.</p></div>` +
  col('Produit', [['/#detection', 'Détection'], ['/#modes', 'Exécution'], ['/#perf', 'Performance'], ['/#greenops', 'GreenOps'], ['/#comparatif', 'Comparatif']]) +
  col('Documentation', [['/guide#quickstart', 'Démarrage rapide'], ['/guide#cli', 'Référence CLI'], ['/guide#config', 'Configuration'], ['/guide#metrics', 'Méthodologie GreenOps']]) +
  col('Projet', [['https://github.com/robintra/perf-sentinel', 'GitHub'], ['https://crates.io/crates/perf-sentinel', 'crates.io'], ['https://docs.rs/perf-sentinel-core', 'docs.rs'], ['/#license', 'Licence AGPL-3.0']]) +
  `</div>` +
  `<div style="display:flex;justify-content:space-between;gap:18px;margin-top:26px;flex-wrap:wrap;font-size:12.5px;color:var(--text-2)">` +
  `<div style="display:flex;flex-direction:column;gap:6px">` +
  `<span>© 2026 perf-sentinel · AGPL-3.0 · Page éco-conçue, sans script de tracking</span>` +
  `<span style="display:flex;gap:14px"><a href="/mentions-legales">Mentions légales</a><a href="/confidentialite">Confidentialité</a></span>` +
  `</div>` +
  `<span>Logo &amp; bannière, <a href="https://www.linkedin.com/in/gwendoline-meignen-b0224873/" target="_blank" rel="noopener" id="ftCredit">Gwendoline Meignen</a></span>` +
  `</div></div></footer>`;

const initScript = `<script>(function(){try{var t=localStorage.getItem('ps-theme');if(t)document.currentScript.parentElement.setAttribute('data-theme',t);}catch(e){}})();</script>`;
const tailScript = `<script>(function(){var r=document.querySelector('[data-ps-root]'),b=document.getElementById('themeBtn'),l=document.getElementById('themeLbl');function s(){l.textContent=r.getAttribute('data-theme')==='dark'?'Clair':'Sombre';}s();b.addEventListener('click',function(){var n=r.getAttribute('data-theme')==='dark'?'light':'dark';r.setAttribute('data-theme',n);try{localStorage.setItem('ps-theme',n);}catch(e){}s();});})();</script>`;

const legalCss = `<style>.legal-main{max-width:760px;margin:0 auto;padding:56px 28px 84px}.legal-main h1{font-size:30px;line-height:1.2;margin:0 0 6px;color:var(--text)}.legal-main h2{font-size:18px;margin:36px 0 10px;color:var(--text)}.legal-main p,.legal-main li{font-size:15px;line-height:1.66;color:var(--text-2)}.legal-main .upd{font-size:13px;opacity:.8;margin:0 0 30px}.legal-main a{color:var(--accent)}.todo{background:color-mix(in srgb,#E8B339 26%,transparent);color:var(--text);padding:1px 6px;border-radius:4px;font-weight:600}</style>`;

const todo = (t) => `<span class="todo">[${t}]</span>`;
const EMAIL = '<a href="mailto:robin.trassard@gmail.com">robin.trassard@gmail.com</a>';

function page(title, desc, main) {
  return `<!doctype html><html lang="fr"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">` +
    `<title>${title} · perf-sentinel</title><meta name="description" content="${desc}"><meta name="robots" content="index,follow">` +
    `<link rel="icon" type="image/svg+xml" href="/assets/favicon.svg"><link rel="stylesheet" href="/fonts/fonts.css">${STYLE}${legalCss}</head>` +
    `<body><div data-ps-root data-theme="dark">${initScript}<div style="min-height:100vh;display:flex;flex-direction:column;background-color:var(--bg);color:var(--text)">` +
    `${header}<main class="legal-main" style="flex:1 0 auto">${main}</main>${footer}</div></div>${tailScript}</body></html>`;
}

const mentions = page('Mentions légales',
  'Mentions légales du site perf-sentinel : éditeur, directeur de la publication et hébergeur.',
  `<h1>Mentions légales</h1><p class="upd">Dernière mise à jour : juin 2026</p>` +
  `<h2>Éditeur du site</h2><p>Le présent site est édité par Robin Trassard, à titre personnel (particulier non-professionnel). Contact : ${EMAIL}.</p>` +
  `<h2>Directeur de la publication</h2><p>Robin Trassard.</p>` +
  `<h2>Hébergeur</h2><p>Le site est hébergé par GitHub Pages — GitHub, Inc., 88 Colin P. Kelly Jr. Street, San Francisco, CA 94107, États-Unis (<a href="https://github.com" target="_blank" rel="noopener">github.com</a>).</p>` +
  `<p>Le nom de domaine perf-sentinel.dev est enregistré auprès d'OVH SAS, 2 rue Kellermann, 59100 Roubaix, France (<a href="https://www.ovhcloud.com" target="_blank" rel="noopener">ovhcloud.com</a>).</p>` +
  `<h2>Propriété intellectuelle</h2><p>Le logiciel perf-sentinel est distribué sous licence libre GNU AGPL-3.0 (<a href="https://www.gnu.org/licenses/agpl-3.0.html" target="_blank" rel="noopener">texte de la licence</a>) ; son code source est disponible sur <a href="https://github.com/robintra/perf-sentinel" target="_blank" rel="noopener">GitHub</a>. Le logo et la bannière sont l'œuvre de Gwendoline Meignen. Les autres contenus du site (textes, mise en page) sont la propriété de l'éditeur, sauf mention contraire.</p>` +
  `<h2>Responsabilité</h2><p>Ce site a une vocation informationnelle. L'éditeur s'efforce de fournir des informations exactes mais ne saurait garantir leur exhaustivité ni l'absence d'erreurs. L'utilisation du logiciel perf-sentinel relève de la seule responsabilité de l'utilisateur, dans les conditions de la licence AGPL-3.0.</p>` +
  `<h2>Données personnelles</h2><p>Ce site ne collecte aucune donnée personnelle et n'utilise aucun cookie ni traceur. Pour le détail, voir la <a href="/confidentialite">politique de confidentialité</a>.</p>`);

const confid = page('Politique de confidentialité',
  'Politique de confidentialité du site perf-sentinel : aucun cookie, aucun traceur, aucune collecte de données.',
  `<h1>Politique de confidentialité</h1><p class="upd">Dernière mise à jour : juin 2026</p>` +
  `<p>La protection de ta vie privée est prise au sérieux. Cette page décrit, en clair, les (rares) données que ce site traite, conformément au Règlement général sur la protection des données (RGPD).</p>` +
  `<h2>Aucune collecte de données</h2><p>Ce site est purement informationnel. Il ne comporte ni formulaire, ni compte utilisateur, ni inscription à une newsletter. Aucune donnée personnelle n'est collectée directement par l'éditeur.</p>` +
  `<h2>Cookies et traceurs</h2><p>Ce site ne dépose aucun cookie et n'utilise aucun traceur ni outil de mesure d'audience (pas de Google Analytics, Matomo, Plausible, etc.). Aucun consentement n'est donc requis et aucun bandeau cookies n'est nécessaire.</p>` +
  `<h2>Stockage local (préférences)</h2><p>Tes préférences d'affichage (thème clair/sombre et langue) sont enregistrées dans le stockage local (localStorage) de ton navigateur. Ces informations restent sur ton appareil, ne sont jamais transmises à un serveur, et tu peux les effacer à tout moment en vidant les données de site de ton navigateur.</p>` +
  `<h2>Polices de caractères</h2><p>Les polices sont hébergées directement sur le site. Aucune requête n'est envoyée à un service tiers (pas de Google Fonts), donc aucune donnée n'est transmise de ce fait.</p>` +
  `<h2>Hébergement et journaux</h2><p>Comme tout hébergeur web, GitHub Pages peut enregistrer des données techniques de connexion (adresse IP, type de navigateur) à des fins de fonctionnement et de sécurité. Ce traitement relève de GitHub ; voir la <a href="https://docs.github.com/site-policy/privacy-policies/github-general-privacy-statement" target="_blank" rel="noopener">politique de confidentialité de GitHub</a>.</p>` +
  `<h2>Liens externes</h2><p>Le site renvoie vers des services tiers (GitHub, crates.io, docs.rs, LinkedIn) qui disposent de leurs propres politiques de confidentialité. L'éditeur n'est pas responsable de leurs pratiques.</p>` +
  `<h2>Tes droits</h2><p>Conformément au RGPD, tu disposes d'un droit d'accès, de rectification et d'effacement de tes données. Le site ne détenant aucune donnée personnelle te concernant, ces demandes concernent surtout l'hébergeur. Pour toute question : ${EMAIL}. Tu peux aussi introduire une réclamation auprès de la CNIL (<a href="https://www.cnil.fr" target="_blank" rel="noopener">cnil.fr</a>).</p>`);

writeFileSync(join(SITE, 'mentions-legales.html'), mentions);
writeFileSync(join(SITE, 'confidentialite.html'), confid);
console.log('wrote mentions-legales.html + confidentialite.html');
