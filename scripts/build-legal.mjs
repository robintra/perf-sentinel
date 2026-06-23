// Generates the standalone legal pages, FR + EN:
//   FR: mentions-legales.html, confidentialite.html
//   EN: legal-notice.html, privacy-policy.html
// Lifts the vitrine <style> verbatim so the chrome (theme vars, fonts, footer,
// hovers) stays identical without duplicating CSS. Each page has a language
// toggle linking to its counterpart and persisting ps-lang.
import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const SITE = join(dirname(fileURLToPath(import.meta.url)), '..');
const STYLE = readFileSync(join(SITE, 'index.html'), 'utf8').match(/<style>[\s\S]*?<\/style>/)[0];

const SUN = '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="var(--accent)" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="4.2"></circle><path d="M12 2.6v2.1M12 19.3v2.1M4.5 4.5l1.5 1.5M18 18l1.5 1.5M2.6 12h2.1M19.3 12h2.1M4.5 19.5l1.5-1.5M18 6l1.5-1.5"></path></svg>';
const MOON = '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="var(--accent)" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 12.8A9 9 0 1 1 11.2 3 7 7 0 0 0 21 12.8z"></path></svg>';
const GH = '<svg viewBox="0 0 24 24" width="17" height="17" fill="currentColor" aria-hidden="true"><path d="M12 .297c-6.63 0-12 5.373-12 12 0 5.303 3.438 9.8 8.205 11.385.6.113.82-.258.82-.577 0-.285-.01-1.04-.015-2.04-3.338.724-4.042-1.61-4.042-1.61C4.422 18.07 3.633 17.7 3.633 17.7c-1.087-.744.084-.729.084-.729 1.205.084 1.838 1.236 1.838 1.236 1.07 1.835 2.809 1.305 3.495.998.108-.776.417-1.305.76-1.605-2.665-.3-5.466-1.332-5.466-5.93 0-1.31.465-2.38 1.235-3.22-.135-.303-.54-1.523.105-3.176 0 0 1.005-.322 3.3 1.23.96-.267 1.98-.399 3-.405 1.02.006 2.04.138 3 .405 2.28-1.552 3.285-1.23 3.285-1.23.645 1.653.24 2.873.12 3.176.765.84 1.23 1.91 1.23 3.22 0 4.61-2.805 5.625-5.475 5.92.42.36.81 1.096.81 2.22 0 1.606-.015 2.896-.015 3.286 0 .315.21.69.825.57C20.565 22.092 24 17.592 24 12.297c0-6.627-5.373-12-12-12"></path></svg>';

const logo = '<img data-logo="light" src="/assets/logo-h-light.svg" alt="perf sentinel" style="height:38px;width:auto"><img data-logo="dark" src="/assets/logo-h-dark.svg" alt="perf sentinel" style="height:38px;width:auto"><img data-logo="light" src="/assets/logo-h-light.svg" alt="perf sentinel" style="height:40px;width:auto;display:none">';

const FT = {
  fr: { tagline: 'Détecteur d’anti-patterns d’I/O au niveau protocole, auto-hébergeable et carbon-aware.', product: 'Produit', docs: 'Documentation', project: 'Projet', detection: 'Détection', execution: 'Exécution', perf: 'Performance', greenops: 'GreenOps', comparison: 'Comparatif', quickstart: 'Démarrage rapide', cli: 'Référence CLI', config: 'Configuration', method: 'Méthodologie GreenOps', license: 'Licence AGPL-3.0', copyright: '© 2026 perf-sentinel · AGPL-3.0 · Page éco-conçue, sans script de tracking', legal: 'Mentions légales', privacy: 'Confidentialité', credit: 'Logo & bannière, ' },
  en: { tagline: 'Protocol-level I/O anti-pattern detector, self-hostable and carbon-aware.', product: 'Product', docs: 'Documentation', project: 'Project', detection: 'Detection', execution: 'Execution', perf: 'Performance', greenops: 'GreenOps', comparison: 'Comparison', quickstart: 'Quickstart', cli: 'CLI reference', config: 'Configuration', method: 'GreenOps methodology', license: 'AGPL-3.0 license', copyright: '© 2026 perf-sentinel · AGPL-3.0 · Eco-designed page, no tracking script', legal: 'Legal notice', privacy: 'Privacy', credit: 'Logo & banner, ' },
};
const LEGAL_HREF = { fr: '/mentions-legales', en: '/legal-notice' };
const PRIV_HREF = { fr: '/confidentialite', en: '/privacy-policy' };
const HOME = { fr: 'Accueil', en: 'Home' };
const LANGSW = { fr: 'EN', en: 'FR' };
const OTHER = { fr: 'en', en: 'fr' };

const PILL = "font-family:'JetBrains Mono',monospace;font-size:12px;letter-spacing:.02em;color:var(--text-2);background:var(--surface);border:1px solid var(--border);border-radius:999px;padding:7px 13px";

function header(lang, otherHref) {
  return `<header style="position:sticky;top:0;z-index:50;background:color-mix(in srgb,var(--bg) 85%,transparent);backdrop-filter:blur(10px);border-bottom:1px solid var(--border)"><div style="max-width:1120px;margin:0 auto;padding:14px 28px;display:flex;align-items:center;gap:18px">` +
    `<a href="/" style="display:flex;align-items:center;flex:none">${logo}</a>` +
    `<a href="/" style="font-size:14px;color:var(--text-2);text-decoration:none">&larr; ${HOME[lang]}</a>` +
    `<div style="margin-left:auto;display:flex;align-items:center;gap:10px">` +
    `<a aria-label="Language" href="${otherHref}" style="${PILL};font-weight:600;letter-spacing:.04em;text-decoration:none;display:inline-flex;align-items:center">${LANGSW[lang]}</a>` +
    `<button id="themeBtn" aria-label="Theme" style="display:flex;align-items:center;gap:7px;${PILL};cursor:pointer"><span class="th-ico th-sun">${SUN}</span><span class="th-ico th-moon">${MOON}</span><span id="themeLbl">${lang === 'fr' ? 'Sombre' : 'Dark'}</span></button>` +
    `<a data-plain href="https://github.com/robintra/perf-sentinel" aria-label="perf-sentinel on GitHub" style="display:flex;align-items:center;gap:8px;font-size:14px;font-weight:600;color:#FFFFFF;background:#24292F;border:1px solid rgba(240,246,252,.18);border-radius:8px;padding:8px 15px">${GH}GitHub</a>` +
    `</div></div></header>`;
}

const col = (title, links) =>
  `<div><div style="font-size:13px;font-weight:600;color:var(--text);margin-bottom:13px">${title}</div><div style="display:flex;flex-direction:column;gap:9px;font-size:14px;color:var(--text-2)">${links.map(([h, t]) => `<a href="${h}">${t}</a>`).join('')}</div></div>`;

function footer(lang) {
  const f = FT[lang];
  return `<footer style="border-top:1px solid var(--border);background:var(--surface-2)"><div style="max-width:1120px;margin:0 auto;padding:48px 28px 36px">` +
    `<div data-grid="ftcols" style="display:grid;grid-template-columns:1.5fr 1fr 1fr 1fr;gap:32px">` +
    `<div><img data-logo="light" src="/assets/logo-h-light.svg" alt="perf sentinel" style="height:40px;width:auto"><img data-logo="dark" src="/assets/logo-h-dark.svg" alt="perf sentinel" style="height:40px;width:auto"><p style="margin:16px 0 0;font-size:13.5px;color:var(--text-2);max-width:240px;line-height:1.55">${f.tagline}</p></div>` +
    col(f.product, [['/#detection', f.detection], ['/#modes', f.execution], ['/#perf', f.perf], ['/#greenops', f.greenops], ['/#comparatif', f.comparison]]) +
    col(f.docs, [['/guide#quickstart', f.quickstart], ['/guide#cli', f.cli], ['/guide#config', f.config], ['/guide#metrics', f.method]]) +
    col(f.project, [['https://github.com/robintra/perf-sentinel', 'GitHub'], ['https://crates.io/crates/perf-sentinel', 'crates.io'], ['https://docs.rs/perf-sentinel-core', 'docs.rs'], ['/#license', f.license]]) +
    `</div>` +
    `<div style="display:flex;justify-content:space-between;gap:18px;margin-top:26px;flex-wrap:wrap;font-size:12.5px;color:var(--text-2)">` +
    `<div style="display:flex;flex-direction:column;gap:6px"><span>${f.copyright}</span><span style="display:flex;gap:14px"><a href="${LEGAL_HREF[lang]}">${f.legal}</a><a href="${PRIV_HREF[lang]}">${f.privacy}</a></span></div>` +
    `<span>${f.credit}<a href="https://www.linkedin.com/in/gwendoline-meignen-b0224873/" target="_blank" rel="noopener" id="ftCredit">Gwendoline Meignen</a></span>` +
    `</div></div></footer>`;
}

const legalCss = `<style>.legal-main{max-width:760px;margin:0 auto;padding:56px 28px 84px}.legal-main h1{font-size:30px;line-height:1.2;margin:0 0 6px;color:var(--text)}.legal-main h2{font-size:18px;margin:36px 0 10px;color:var(--text)}.legal-main p,.legal-main li{font-size:15px;line-height:1.66;color:var(--text-2)}.legal-main .upd{font-size:13px;opacity:.8;margin:0 0 30px}.legal-main a{color:var(--accent-strong)}[data-ps-root]:not([data-theme="dark"]) [style*="color:var(--accent)"],[data-ps-root]:not([data-theme="dark"]) [style*="color: var(--accent)"],[data-ps-root]:not([data-theme="dark"]) [style*="color:#0ba671"],[data-ps-root]:not([data-theme="dark"]) [style*="color: #0ba671"]{color:var(--accent-strong)!important}</style>`;

const initScript = `<script>(function(){try{var t=localStorage.getItem('ps-theme');if(t)document.currentScript.parentElement.setAttribute('data-theme',t);}catch(e){}})();</script>`;
const tailScript = (lang) => {
  const words = lang === 'fr' ? "==='dark'?'Clair':'Sombre'" : "==='dark'?'Light':'Dark'";
  return `<script>(function(){var r=document.querySelector('[data-ps-root]'),b=document.getElementById('themeBtn'),l=document.getElementById('themeLbl');function s(){l.textContent=r.getAttribute('data-theme')${words};}s();b.addEventListener('click',function(){var n=r.getAttribute('data-theme')==='dark'?'light':'dark';r.setAttribute('data-theme',n);try{localStorage.setItem('ps-theme',n);}catch(e){}s();});var lb=document.querySelector('[aria-label="Language"]');if(lb)lb.addEventListener('click',function(){try{localStorage.setItem('ps-lang','${OTHER[lang]}');}catch(e){}});})();</script>`;
};

function page(lang, otherHref, title, desc, main) {
  return `<!doctype html><html lang="${lang}"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">` +
    `<title>${title} · perf-sentinel</title><meta name="description" content="${desc}"><meta name="robots" content="index,follow">` +
    `<meta property="og:type" content="website"><meta property="og:title" content="${title} · perf-sentinel"><meta property="og:description" content="${desc}"><meta property="og:image" content="https://perf-sentinel.dev/assets/og-banner.png"><meta property="og:image:width" content="1200"><meta property="og:image:height" content="427"><meta property="og:image:alt" content="perf-sentinel"><meta name="theme-color" content="#0BA671"><meta name="twitter:card" content="summary_large_image"><meta name="twitter:image" content="https://perf-sentinel.dev/assets/og-banner.png">` +
    `<link rel="icon" type="image/svg+xml" href="/assets/favicon.svg"><link rel="stylesheet" href="/fonts/fonts.css">${STYLE}${legalCss}</head>` +
    `<body><div data-ps-root data-theme="light">${initScript}<div style="min-height:100vh;display:flex;flex-direction:column;background-color:var(--bg);color:var(--text)">` +
    `${header(lang, otherHref)}<main class="legal-main" style="flex:1 0 auto">${main}</main>${footer(lang)}</div></div>${tailScript(lang)}</body></html>`;
}

const EMAIL = '<a href="mailto:robin.trassard@gmail.com">robin.trassard@gmail.com</a>';

// --- content ---
const mentionsFR =
  `<h1>Mentions légales</h1><p class="upd">Dernière mise à jour : juin 2026</p>` +
  `<h2>Éditeur du site</h2><p>Le présent site est édité par Robin Trassard, à titre personnel (particulier non-professionnel).<br>Contact : ${EMAIL}.</p>` +
  `<h2>Directeur de la publication</h2><p>Robin Trassard.</p>` +
  `<h2>Hébergeur</h2><p>Le site est hébergé par GitHub Pages, service de GitHub, Inc., 88 Colin P. Kelly Jr. Street, San Francisco, CA 94107, États-Unis (<a href="https://github.com" target="_blank" rel="noopener">github.com</a>).</p>` +
  `<p>Le nom de domaine perf-sentinel.dev est enregistré auprès d'OVH SAS, 2 rue Kellermann, 59100 Roubaix, France (<a href="https://www.ovhcloud.com" target="_blank" rel="noopener">ovhcloud.com</a>).</p>` +
  `<h2>Propriété intellectuelle</h2><p>Le logiciel perf-sentinel est distribué sous licence libre GNU AGPL-3.0 (<a href="https://www.gnu.org/licenses/agpl-3.0.html" target="_blank" rel="noopener">texte de la licence</a>) ; son code source est disponible sur <a href="https://github.com/robintra/perf-sentinel" target="_blank" rel="noopener">GitHub</a>. Le logo et la bannière sont l'œuvre de Gwendoline Meignen. Les autres contenus du site (textes, mise en page) sont la propriété de l'éditeur, sauf mention contraire.</p>` +
  `<h2>Responsabilité</h2><p>Ce site a une vocation informationnelle. L'éditeur s'efforce de fournir des informations exactes mais ne saurait garantir leur exhaustivité ni l'absence d'erreurs. L'utilisation du logiciel perf-sentinel relève de la seule responsabilité de l'utilisateur, dans les conditions de la licence AGPL-3.0.</p>` +
  `<h2>Données personnelles</h2><p>Ce site ne collecte aucune donnée personnelle et n'utilise aucun cookie ni traceur. Pour le détail, voir la <a href="/confidentialite">politique de confidentialité</a>.</p>`;

const mentionsEN =
  `<h1>Legal notice</h1><p class="upd">Last updated: June 2026</p>` +
  `<h2>Publisher</h2><p>This site is published by Robin Trassard, as a private individual (non-professional).<br>Contact: ${EMAIL}.</p>` +
  `<h2>Publication director</h2><p>Robin Trassard.</p>` +
  `<h2>Host</h2><p>This site is hosted by GitHub Pages, a service of GitHub, Inc., 88 Colin P. Kelly Jr. Street, San Francisco, CA 94107, USA (<a href="https://github.com" target="_blank" rel="noopener">github.com</a>).</p>` +
  `<p>The perf-sentinel.dev domain name is registered with OVH SAS, 2 rue Kellermann, 59100 Roubaix, France (<a href="https://www.ovhcloud.com" target="_blank" rel="noopener">ovhcloud.com</a>).</p>` +
  `<h2>Intellectual property</h2><p>The perf-sentinel software is distributed under the GNU AGPL-3.0 free license (<a href="https://www.gnu.org/licenses/agpl-3.0.html" target="_blank" rel="noopener">license text</a>); its source code is available on <a href="https://github.com/robintra/perf-sentinel" target="_blank" rel="noopener">GitHub</a>. The logo and banner are the work of Gwendoline Meignen. Other site content (text, layout) is the property of the publisher unless otherwise stated.</p>` +
  `<h2>Liability</h2><p>This site is informational. The publisher strives to provide accurate information but cannot guarantee its completeness or the absence of errors. Use of the perf-sentinel software is the sole responsibility of the user, under the terms of the AGPL-3.0 license.</p>` +
  `<h2>Personal data</h2><p>This site collects no personal data and uses no cookie or tracker. For details, see the <a href="/privacy-policy">privacy policy</a>.</p>`;

const confidFR =
  `<h1>Politique de confidentialité</h1><p class="upd">Dernière mise à jour : juin 2026</p>` +
  `<p>La protection de ta vie privée est prise au sérieux. Cette page décrit, en clair, les (rares) données que ce site traite, conformément au Règlement général sur la protection des données (RGPD).</p>` +
  `<h2>Aucune collecte de données</h2><p>Ce site est purement informationnel. Il ne comporte ni formulaire, ni compte utilisateur, ni inscription à une newsletter. Aucune donnée personnelle n'est collectée directement par l'éditeur.</p>` +
  `<h2>Cookies et traceurs</h2><p>Ce site ne dépose aucun cookie et n'utilise aucun traceur ni outil de mesure d'audience (pas de Google Analytics, Matomo, Plausible, etc.). Aucun consentement n'est donc requis et aucun bandeau cookies n'est nécessaire.</p>` +
  `<h2>Stockage local (préférences)</h2><p>Tes préférences d'affichage (thème clair/sombre et langue) sont enregistrées dans le stockage local (localStorage) de ton navigateur. Ces informations restent sur ton appareil, ne sont jamais transmises à un serveur, et tu peux les effacer à tout moment en vidant les données de site de ton navigateur.</p>` +
  `<h2>Polices de caractères</h2><p>Les polices sont hébergées directement sur le site. Aucune requête n'est envoyée à un service tiers (pas de Google Fonts), donc aucune donnée n'est transmise de ce fait.</p>` +
  `<h2>Hébergement et journaux</h2><p>Comme tout hébergeur web, GitHub Pages peut enregistrer des données techniques de connexion (adresse IP, type de navigateur) à des fins de fonctionnement et de sécurité. Ce traitement relève de GitHub ; voir la <a href="https://docs.github.com/site-policy/privacy-policies/github-general-privacy-statement" target="_blank" rel="noopener">politique de confidentialité de GitHub</a>.</p>` +
  `<h2>Liens externes</h2><p>Le site renvoie vers des services tiers (GitHub, crates.io, docs.rs, LinkedIn) qui disposent de leurs propres politiques de confidentialité. L'éditeur n'est pas responsable de leurs pratiques.</p>` +
  `<h2>Tes droits</h2><p>Conformément au RGPD, tu disposes d'un droit d'accès, de rectification et d'effacement de tes données. Le site ne détenant aucune donnée personnelle te concernant, ces demandes concernent surtout l'hébergeur. Pour toute question : ${EMAIL}. Tu peux aussi introduire une réclamation auprès de la CNIL (<a href="https://www.cnil.fr" target="_blank" rel="noopener">cnil.fr</a>).</p>`;

const confidEN =
  `<h1>Privacy policy</h1><p class="upd">Last updated: June 2026</p>` +
  `<p>Your privacy is taken seriously. This page describes, in plain terms, the (few) data this site processes, in line with the General Data Protection Regulation (GDPR).</p>` +
  `<h2>No data collection</h2><p>This site is purely informational. It has no form, no user account, and no newsletter signup. No personal data is collected directly by the publisher.</p>` +
  `<h2>Cookies and trackers</h2><p>This site sets no cookie and uses no tracker or audience-measurement tool (no Google Analytics, Matomo, Plausible, etc.). No consent is therefore required and no cookie banner is needed.</p>` +
  `<h2>Local storage (preferences)</h2><p>Your display preferences (light/dark theme and language) are stored in your browser's local storage. This information stays on your device, is never sent to a server, and you can clear it at any time by clearing the site data in your browser.</p>` +
  `<h2>Fonts</h2><p>Fonts are hosted directly on the site. No request is sent to a third-party service (no Google Fonts), so no data is transmitted as a result.</p>` +
  `<h2>Hosting and logs</h2><p>Like any web host, GitHub Pages may record technical connection data (IP address, browser type) for operational and security purposes. This processing is GitHub's; see <a href="https://docs.github.com/site-policy/privacy-policies/github-general-privacy-statement" target="_blank" rel="noopener">GitHub's privacy statement</a>.</p>` +
  `<h2>External links</h2><p>The site links to third-party services (GitHub, crates.io, docs.rs, LinkedIn) which have their own privacy policies. The publisher is not responsible for their practices.</p>` +
  `<h2>Your rights</h2><p>Under the GDPR, you have the right to access, rectify and erase your data. As the site holds no personal data about you, such requests mainly concern the host. For any question: ${EMAIL}. You may also lodge a complaint with the CNIL, the French data-protection authority (<a href="https://www.cnil.fr" target="_blank" rel="noopener">cnil.fr</a>).</p>`;

writeFileSync(join(SITE, 'mentions-legales.html'), page('fr', '/legal-notice', 'Mentions légales', 'Mentions légales du site perf-sentinel : éditeur, directeur de la publication et hébergeur.', mentionsFR));
writeFileSync(join(SITE, 'legal-notice.html'), page('en', '/mentions-legales', 'Legal notice', 'Legal notice for the perf-sentinel site: publisher, publication director and host.', mentionsEN));
writeFileSync(join(SITE, 'confidentialite.html'), page('fr', '/privacy-policy', 'Politique de confidentialité', 'Politique de confidentialité du site perf-sentinel : aucun cookie, aucun traceur, aucune collecte de données.', confidFR));
writeFileSync(join(SITE, 'privacy-policy.html'), page('en', '/confidentialite', 'Privacy policy', 'Privacy policy for the perf-sentinel site: no cookies, no trackers, no data collection.', confidEN));
console.log('wrote mentions-legales + legal-notice + confidentialite + privacy-policy');
