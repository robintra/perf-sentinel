/* Minimal Markdown renderer tailored for the perf-sentinel docs.
   window.PSMD.render(markdown, {id, lang, theme}) -> { html, toc } */
(function () {
  function esc(s) {
    return String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
  }
  function slug(txt) {
    return String(txt)
      .replace(/`/g, '')
      .replace(/\*\*?/g, '')
      .replace(/\[([^\]]+)\]\([^)]+\)/g, '$1')
      .toLowerCase()
      .replace(/[^\p{L}\p{N}\s-]/gu, '')
      .trim()
      .replace(/\s/g, '-');
  }

  // ---- syntax highlight -------------------------------------------------
  var SUB = { demo:1, analyze:1, watch:1, report:1, diff:1, explain:1, inspect:1, query:1, ack:1, calibrate:1, disclose:1, tempo:1, 'jaeger-query':1, 'pg-stat':1, 'verify-hash':1, 'hash-bake':1, bench:1, man:1, completions:1, create:1, revoke:1, list:1 };
  function C(v, s) { return '<span style="color:var(' + v + ')">' + s + '</span>'; }
  function hlBash(code) {
    return code.split('\n').map(function (line) {
      if (line.trim() === '') return '';
      if (line.trim().charAt(0) === '#') return '<span style="color:var(--term-comment)">' + esc(line) + '</span>';
      var codePart = line, comm = '';
      var ci = line.indexOf(' #');
      if (ci >= 0) { codePart = line.slice(0, ci); comm = line.slice(ci); }
      var w = 0;
      var html = codePart.split(/(\s+)/).map(function (tok) {
        if (tok === '' || /^\s+$/.test(tok)) return esc(tok);
        var e = esc(tok);
        if (tok === '\\' || tok === '|' || tok === '>' || tok === '&&') return e;
        if (tok.charAt(0) === '-') return C('--code-flag', e);
        w++;
        if (w === 1) return '<span style="color:var(--code-cmd);font-weight:600">' + e + '</span>';
        if (w === 2 && SUB[tok]) return C('--code-sub', e);
        if (/^["'].*["']$/.test(tok)) return C('--code-num', e);
        if (/^:?[0-9]/.test(tok)) return C('--code-num', e);
        return e;
      }).join('');
      return html + (comm ? '<span style="color:var(--term-comment)">' + esc(comm) + '</span>' : '');
    }).join('\n');
  }
  function hlToml(code) {
    return code.split('\n').map(function (line) {
      if (line.trim() === '') return '';
      var t = line.trim();
      if (t.charAt(0) === '#') return '<span style="color:var(--term-comment)">' + esc(line) + '</span>';
      if (t.charAt(0) === '[') return '<span style="color:var(--code-cmd);font-weight:600">' + esc(line) + '</span>';
      var codePart = line, comm = '';
      var ci = line.indexOf('#');
      if (ci >= 0) { codePart = line.slice(0, ci); comm = line.slice(ci); }
      var eq = codePart.indexOf('=');
      var body;
      if (eq >= 0) {
        body = C('--code-sub', esc(codePart.slice(0, eq))) + C('--term-dim', '=') + C('--code-num', esc(codePart.slice(eq + 1)));
      } else { body = esc(codePart); }
      return body + (comm ? '<span style="color:var(--term-comment)">' + esc(comm) + '</span>' : '');
    }).join('\n');
  }
  function hlJson(code) {
    var e = esc(code).replace(/"/g, '&quot;');
    e = e.replace(/(&quot;(?:[^&]|&(?!quot;))*?&quot;)(\s*:)/g, C('--code-sub', '$1') + '$2');
    e = e.replace(/:(\s*)(&quot;(?:[^&]|&(?!quot;))*?&quot;)/g, ':$1' + C('--code-num', '$2'));
    e = e.replace(/\b(true|false|null)\b/g, C('--code-flag', '$1'));
    e = e.replace(/(:\s*)(-?\d+(?:\.\d+)?)/g, '$1' + C('--code-num', '$2'));
    return e;
  }
  var RUST_KW = { as:1,async:1,await:1,break:1,'const':1,'continue':1,crate:1,dyn:1,'else':1,'enum':1,'extern':1,fn:1,'for':1,'if':1,impl:1,'in':1,let:1,loop:1,match:1,mod:1,move:1,mut:1,pub:1,ref:1,'return':1,'self':1,Self:1,'static':1,struct:1,'super':1,trait:1,type:1,unsafe:1,use:1,where:1,'while':1,bool:1,'true':1,'false':1,Some:1,None:1,Ok:1,Err:1 };
  var CS_KW = { using:1,namespace:1,'class':1,struct:1,'interface':1,'enum':1,'public':1,'private':1,'protected':1,'internal':1,'static':1,'void':1,var:1,'new':1,'return':1,'if':1,'else':1,'for':1,foreach:1,'while':1,'switch':1,'case':1,'break':1,'continue':1,async:1,await:1,'this':1,base:1,'null':1,'true':1,'false':1,string:1,'int':1,bool:1,'double':1,'float':1,'long':1,override:1,virtual:1,abstract:1,readonly:1,'const':1,get:1,set:1,'in':1,out:1,ref:1,typeof:1,is:1,as:1,'throw':1,'try':1,'catch':1,'finally':1 };
  function hlDiff(code) { return code.split('\n').map(function (l) { var c = l.charAt(0), e = esc(l); if (c === '+') return C('--code-cmd', e); if (c === '-') return C('--coral', e); if (c === '@') return C('--code-flag', e); if (c === '#') return C('--term-comment', e); return e; }).join('\n'); }
  function hlDockerfile(code) { var KW = /^(from|run|cmd|label|expose|env|add|copy|entrypoint|volume|user|workdir|arg|onbuild|stopsignal|healthcheck|shell|maintainer)$/i; return code.split('\n').map(function (l) { if (l.trim().charAt(0) === '#') return C('--term-comment', esc(l)); var m = l.match(/^(\s*)([A-Za-z]+)([\s\S]*)$/); if (m && KW.test(m[2])) return esc(m[1]) + '<span style="color:var(--code-cmd);font-weight:600">' + esc(m[2]) + '</span>' + esc(m[3]); return esc(l); }).join('\n'); }
  function hlProps(code) { return code.split('\n').map(function (l) { var t = l.trim(); if (t.charAt(0) === '#' || t.charAt(0) === ';') return C('--term-comment', esc(l)); var eq = l.search(/[=:]/); if (eq >= 0) return C('--code-sub', esc(l.slice(0, eq))) + C('--term-dim', esc(l.slice(eq, eq + 1))) + C('--code-num', esc(l.slice(eq + 1))); return esc(l); }).join('\n'); }
  function hlXml(code) { var e = esc(code).replace(/"/g, '&quot;'); e = e.replace(/&lt;!--[\s\S]*?--&gt;/g, function (m) { return C('--term-comment', m); }); e = e.replace(/(&lt;\/?)([\w:.-]+)/g, function (m, b, n) { return b + C('--code-cmd', n); }); e = e.replace(/([\w:.-]+)(=)(&quot;[^&]*?&quot;)/g, function (m, a, q, v) { return C('--code-sub', a) + q + C('--code-num', v); }); return e; }
  function hlCLike(code, KW) {
    var re = /(\/\/[^\n]*|\/\*[\s\S]*?\*\/)|("(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])')|(#!?\[[^\]]*\])|(\b\d[\d_]*(?:\.[\d_]+)?(?:[iuf]\d+)?\b)|([A-Za-z_][A-Za-z0-9_]*)(!?)/g;
    var out = '', last = 0;
    code.replace(re, function (m, comment, str, attr, num, ident, bang, offset) {
      out += esc(code.slice(last, offset));
      last = offset + m.length;
      if (comment) out += C('--term-comment', esc(comment));
      else if (str) out += C('--code-num', esc(str));
      else if (attr) out += C('--term-comment', esc(attr));
      else if (num) out += C('--code-num', esc(num));
      else if (ident !== undefined && ident !== '') {
        if (bang) out += C('--code-flag', esc(ident + bang));
        else if (KW[ident]) out += '<span style="color:var(--code-cmd);font-weight:600">' + esc(ident) + '</span>';
        else if (/^[A-Z]/.test(ident)) out += C('--code-sub', esc(ident));
        else out += esc(ident);
      } else out += esc(m);
      return m;
    });
    out += esc(code.slice(last));
    return out;
  }
  var RUBY_KW = { 'def':1,'end':1,'do':1,'class':1,'module':1,'require':1,'require_relative':1,'load':1,'gem':1,'if':1,'elsif':1,'else':1,'unless':1,'case':1,'when':1,'then':1,'while':1,'until':1,'for':1,'in':1,'begin':1,'rescue':1,'ensure':1,'retry':1,'raise':1,'return':1,'next':1,'break':1,'yield':1,'super':1,'self':1,'nil':1,'true':1,'false':1,'and':1,'or':1,'not':1,'lambda':1,'proc':1,'new':1,'attr_accessor':1,'attr_reader':1,'attr_writer':1 };
  function hlRuby(code) {
    var re = /(#[^\n]*)|("(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*')|(:[A-Za-z_]\w*[?!]?)|(\b\d[\d_]*(?:\.[\d_]+)?\b)|([A-Za-z_]\w*[?!]?)/g;
    var out = '', last = 0;
    code.replace(re, function (m, comment, str, sym, num, ident, offset) {
      out += esc(code.slice(last, offset));
      last = offset + m.length;
      if (comment) out += C('--term-comment', esc(comment));
      else if (str) out += C('--code-num', esc(str));
      else if (sym) out += C('--code-sub', esc(sym));
      else if (num) out += C('--code-num', esc(num));
      else if (ident) {
        if (RUBY_KW[ident]) out += '<span style="color:var(--code-cmd);font-weight:600">' + esc(ident) + '</span>';
        else if (/^[A-Z]/.test(ident)) out += C('--code-sub', esc(ident));
        else out += esc(ident);
      } else out += esc(m);
      return m;
    });
    out += esc(code.slice(last));
    return out;
  }
  var GROOVY_KW = { 'abstract':1,'as':1,'assert':1,'boolean':1,'break':1,'byte':1,'case':1,'catch':1,'char':1,'class':1,'def':1,'default':1,'do':1,'double':1,'else':1,'enum':1,'extends':1,'false':1,'final':1,'finally':1,'float':1,'for':1,'if':1,'implements':1,'import':1,'in':1,'instanceof':1,'int':1,'interface':1,'long':1,'new':1,'null':1,'package':1,'private':1,'protected':1,'public':1,'return':1,'short':1,'static':1,'super':1,'switch':1,'synchronized':1,'this':1,'throw':1,'throws':1,'trait':1,'true':1,'try':1,'var':1,'void':1,'while':1 };
  var PHP_KW = { 'abstract':1,'and':1,'array':1,'as':1,'break':1,'callable':1,'case':1,'catch':1,'class':1,'clone':1,'const':1,'continue':1,'declare':1,'default':1,'do':1,'echo':1,'else':1,'elseif':1,'empty':1,'enum':1,'extends':1,'final':1,'finally':1,'fn':1,'for':1,'foreach':1,'function':1,'global':1,'goto':1,'if':1,'implements':1,'include':1,'include_once':1,'instanceof':1,'insteadof':1,'interface':1,'isset':1,'list':1,'match':1,'namespace':1,'new':1,'or':1,'print':1,'private':1,'protected':1,'public':1,'readonly':1,'require':1,'require_once':1,'return':1,'static':1,'switch':1,'throw':1,'trait':1,'try':1,'unset':1,'use':1,'var':1,'while':1,'xor':1,'yield':1,'true':1,'false':1,'null':1,'self':1,'parent':1,'this':1 };
  function hlPHP(code) {
    var re = /(\/\/[^\n]*|#[^\n]*|\/\*[\s\S]*?\*\/)|("(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*')|(\$[A-Za-z_]\w*)|(\b\d[\d_]*(?:\.[\d_]+)?\b)|([A-Za-z_]\w*)/g;
    var out = '', last = 0;
    code.replace(re, function (m, comment, str, variable, num, ident, offset) {
      out += esc(code.slice(last, offset));
      last = offset + m.length;
      if (comment) out += C('--term-comment', esc(comment));
      else if (str) out += C('--code-num', esc(str));
      else if (variable) out += C('--code-sub', esc(variable));
      else if (num) out += C('--code-num', esc(num));
      else if (ident) {
        if (PHP_KW[ident]) out += '<span style="color:var(--code-cmd);font-weight:600">' + esc(ident) + '</span>';
        else if (/^[A-Z]/.test(ident)) out += C('--code-sub', esc(ident));
        else out += esc(ident);
      } else out += esc(m);
      return m;
    });
    out += esc(code.slice(last));
    return out;
  }
  function highlight(lang, code) {
    lang = (lang || '').toLowerCase();
    if (lang === 'rust' || lang === 'rs') return hlCLike(code, RUST_KW);
    if (lang === 'csharp' || lang === 'cs' || lang === 'c#') return hlCLike(code, CS_KW);
    if (lang === 'groovy' || lang === 'gradle') return hlCLike(code, GROOVY_KW);
    if (lang === 'ruby' || lang === 'rb') return hlRuby(code);
    if (lang === 'php') return hlPHP(code);
    if (lang === 'diff') return hlDiff(code);
    if (lang === 'dockerfile' || lang === 'docker') return hlDockerfile(code);
    if (lang === 'properties' || lang === 'ini') return hlProps(code);
    if (lang === 'xml' || lang === 'html') return hlXml(code);
    if (lang === 'bash' || lang === 'sh' || lang === 'shell' || lang === 'console') return hlBash(code);
    if (lang === 'toml') return hlToml(code);
    if (lang === 'json') return hlJson(code);
    if (lang === 'yaml' || lang === 'yml') {
      return code.split('\n').map(function (l) {
        if (l.trim().charAt(0) === '#') return '<span style="color:var(--term-comment)">' + esc(l) + '</span>';
        return esc(l).replace(/^(\s*[\w.-]+)(:)/, C('--code-sub', '$1') + '$2');
      }).join('\n');
    }
    return esc(code);
  }

  // ---- inline -----------------------------------------------------------
  function resolveLink(href, ctx) {
    var anchor = '';
    var hash = href.indexOf('#');
    if (hash >= 0) { anchor = href.slice(hash + 1); href = href.slice(0, hash); }
    if (href === '') return { internal: true, id: ctx.id, anchor: anchor };
    if (/^https?:\/\//.test(href) || /^mailto:/.test(href)) return { external: true, href: href + (anchor ? '#' + anchor : '') };
    if (/\.md$/i.test(href)) {
      var curDir = ctx.id.indexOf('/') >= 0 ? ctx.id.replace(/\/[^/]*$/, '') : '';
      var segs = curDir ? curDir.split('/') : [];
      href.replace(/^\.\//, '').split('/').forEach(function (seg) {
        if (seg === '..') segs.pop();
        else if (seg === '.' || seg === '') { }
        else segs.push(seg);
      });
      var id = segs.join('/').replace(/\.md$/i, '').replace(/-FR$/, '');
      return { internal: true, id: id, anchor: anchor };
    }
    // other relative resources -> GitHub blob
    return { external: true, href: 'https://github.com/robintra/perf-sentinel/blob/main/docs/' + href.replace(/^\.\//, '') + (anchor ? '#' + anchor : '') };
  }
  function inline(text, ctx) {
    var codes = [];
    text = text.replace(/`([^`]+)`/g, function (m, c) { codes.push(c); return '\u0000' + (codes.length - 1) + '\u0000'; });
    text = esc(text);
    text = text.replace(/!\[([^\]]*)\]\(([^)]+)\)/g, function (m, a, u) { return '<em>[' + esc(a) + ']</em>'; });
    text = text.replace(/\[([^\]]+)\]\(([^)]+)\)/g, function (m, t, href) {
      var r = resolveLink(href, ctx);
      if (r.external) return '<a href="' + r.href + '" target="_blank" rel="noopener" class="ps-ext">' + t + '</a>';
      var plain = t.replace(/\u0000(\d+)\u0000/g, function (_, n) { return codes[n]; }).trim();
      var label = t;
      if ((/\.md$/i.test(plain) || /\/$/.test(plain)) && ctx.label) label = esc(ctx.label(r.id));
      return '<a href="#/' + r.id + '" data-doc="' + r.id + '"' + (r.anchor ? ' data-anchor="' + r.anchor + '"' : '') + '>' + label + '</a>';
    });
    text = text.replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>').replace(/__([^_]+)__/g, '<strong>$1</strong>');
    text = text.replace(/(^|[^*\w])\*([^*\s][^*]*?)\*(?!\w)/g, '$1<em>$2</em>');
    text = text.replace(/\u0000(\d+)\u0000/g, function (m, n) { return '<code class="ps-ic">' + esc(codes[n]) + '</code>'; });
    return text;
  }

  // ---- media ------------------------------------------------------------
  // Several provided mermaid SVG exports lost their node labels (only the title
  // survives), so they render as empty boxes. Show the SVG only when it actually
  // carries its labels; otherwise fall back to the ASCII diagram in the markdown.
  var RAW_DIAGRAMS = 'https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/';
  function isGoodDiagram(url) { return /\/diagrams\/svg\//.test(url); }
  function nextIsGoodDiagram(lines, i) {
    var j = i; while (j < lines.length && lines[j].trim() === '') j++;
    if (j >= lines.length) return false;
    var url = null, l = lines[j];
    if (/^\s*<picture>/.test(l)) {
      var k = j, blk = [];
      while (k < lines.length) { blk.push(lines[k]); if (/<\/picture>/.test(lines[k])) break; k++; }
      var m = blk.join(' ').match(/\/diagrams\/svg\/[\w.-]+\.svg/); if (m) url = m[0];
    } else {
      var im = l.trim().match(/^!\[[^\]]*\]\(([^)]+\.svg)\)$/); if (im) url = im[1];
    }
    return url ? isGoodDiagram(url) : false;
  }
  function diagramFrom(url, ctx) {
    var base = url.split('/').pop().replace(/_dark\.svg$/i, '.svg');
    var src = RAW_DIAGRAMS + (ctx.theme === 'dark' ? base.replace(/\.svg$/i, '_dark.svg') : base);
    return '<img class="ps-diagram" src="' + src + '" alt="" loading="lazy">';
  }
  function imageBlock(alt, url, ctx) {
    if (/\/diagrams\/svg\//.test(url)) return isGoodDiagram(url) ? '<figure class="ps-fig">' + diagramFrom(url, ctx) + '</figure>' : '';
    // heavy raster (gif/png) -> click to load
    return '<figure class="ps-media"><button type="button" class="ps-media-btn" data-src="' + esc(url) + '"><span class="ps-media-play">\u25B6</span> ' + esc(alt || 'Load media') + '</button></figure>';
  }
  function pictureBlock(block, ctx) {
    var m = block.match(/\/diagrams\/svg\/[\w.-]+\.svg/);
    if (m) return isGoodDiagram(m[0]) ? '<figure class="ps-fig">' + diagramFrom(m[0], ctx) + '</figure>' : '';
    var im = block.match(/src="([^"]+)"/);
    if (im) return imageBlock('', im[1], ctx);
    return '';
  }

  function codeBlock(lang, code) {
    var label = lang ? '<div class="ps-code-lang">' + esc(lang) + '</div>' : '';
    return '<div class="ps-code">' + label + '<pre><code>' + highlight(lang, code) + '</code></pre></div>';
  }

  function isBlockStart(line) {
    return /^(#{1,6})\s/.test(line) || /^```/.test(line) || /^\s*([-*+]|\d+\.)\s+/.test(line) ||
      /^\s*>\s?/.test(line) || /^(\s*)(-{3,}|\*{3,}|_{3,})\s*$/.test(line) ||
      /^\s*<(picture|details|summary|table|div|img|sub|sup|br|hr)\b/i.test(line) ||
      /^!\[[^\]]*\]\([^)]+\)\s*$/.test(line.trim());
  }

  function parseTable(lines, i, out, ctx) {
    var header = lines[i], sep = lines[i + 1];
    function cells(row) {
      var r = row.trim().replace(/^\|/, '').replace(/\|$/, '');
      return r.split('|').map(function (c) { return c.trim(); });
    }
    var aligns = cells(sep).map(function (c) {
      var l = c.charAt(0) === ':', rg = c.charAt(c.length - 1) === ':';
      return rg && l ? 'center' : rg ? 'right' : 'left';
    });
    var head = cells(header);
    var html = '<div class="ps-tablewrap"><table><thead><tr>';
    head.forEach(function (c, k) { html += '<th style="text-align:' + (aligns[k] || 'left') + '">' + inline(c, ctx) + '</th>'; });
    html += '</tr></thead><tbody>';
    var j = i + 2;
    for (; j < lines.length; j++) {
      if (lines[j].indexOf('|') < 0 || lines[j].trim() === '') break;
      var rc = cells(lines[j]);
      html += '<tr>';
      rc.forEach(function (c, k) { html += '<td style="text-align:' + (aligns[k] || 'left') + '">' + inline(c, ctx) + '</td>'; });
      html += '</tr>';
    }
    html += '</tbody></table></div>';
    out.push(html);
    return j;
  }

  function parseList(lines, i, out, ctx) {
    var stack = [];
    var startIndent = -1;
    var j = i;
    function close(toLen) { while (stack.length > toLen) { var s = stack.pop(); out.push(s.ordered ? '</ol>' : '</ul>'); } }
    for (; j < lines.length; j++) {
      var line = lines[j];
      if (line.trim() === '') {
        if (j + 1 < lines.length && /^\s*([-*+]|\d+\.)\s+/.test(lines[j + 1])) continue;
        break;
      }
      var m = line.match(/^(\s*)([-*+]|\d+\.)\s+(.*)$/);
      if (!m) {
        // continuation line of previous item
        if (stack.length) { out[out.length - 1] = out[out.length - 1].replace(/<\/li>$/, ' ' + inline(line.trim(), ctx) + '</li>'); continue; }
        break;
      }
      var indent = m[1].length;
      var ordered = /\d/.test(m[2]);
      if (startIndent < 0) startIndent = indent;
      var depth = Math.floor((indent - startIndent) / 2);
      if (depth < 0) depth = 0;
      if (depth + 1 > stack.length) { stack.push({ ordered: ordered }); out.push(ordered ? '<ol>' : '<ul>'); }
      else if (depth + 1 < stack.length) { close(depth + 1); }
      out.push('<li>' + inline(m[3], ctx) + '</li>');
    }
    close(0);
    return j;
  }

  function render(md, ctx) {
    md = String(md).replace(/\r\n/g, '\n');
    var lines = md.split('\n');
    var out = [], toc = [], i = 0;
    while (i < lines.length) {
      var line = lines[i];
      var fence = line.match(/^```(\s*([\w-]+))?\s*$/);
      if (fence) {
        var lang = fence[2] || ''; i++;
        var buf = [];
        while (i < lines.length && !/^```\s*$/.test(lines[i])) { buf.push(lines[i]); i++; }
        i++;
        if (!lang && nextIsGoodDiagram(lines, i)) { continue; }
        out.push(codeBlock(lang, buf.join('\n')));
        continue;
      }
      if (/^\s*<picture>/.test(line)) {
        var pb = [line]; i++;
        while (i < lines.length && !/<\/picture>/.test(lines[i - 1])) { pb.push(lines[i]); i++; }
        out.push(pictureBlock(pb.join('\n'), ctx));
        continue;
      }
      var imgm = line.trim().match(/^!\[([^\]]*)\]\(([^)]+)\)$/);
      if (imgm) { out.push(imageBlock(imgm[1], imgm[2], ctx)); i++; continue; }
      var h = line.match(/^(#{1,6})\s+(.*?)\s*#*\s*$/);
      if (h) {
        var lvl = h[1].length, txt = h[2], id = slug(txt);
        if (lvl >= 2 && lvl <= 3) toc.push({ lvl: lvl, txt: txt.replace(/`/g, ''), id: id });
        out.push('<h' + lvl + ' id="' + id + '" class="ps-h' + lvl + '">' + inline(txt, ctx) + '</h' + lvl + '>');
        i++; continue;
      }
      if (/^(\s*)(-{3,}|\*{3,}|_{3,})\s*$/.test(line)) { out.push('<hr class="ps-hr">'); i++; continue; }
      if (line.indexOf('|') >= 0 && i + 1 < lines.length && /^\s*\|?[\s:-]*-{2,}[\s:|-]*$/.test(lines[i + 1]) && lines[i + 1].indexOf('|') >= 0) {
        i = parseTable(lines, i, out, ctx); continue;
      }
      if (/^\s*>\s?/.test(line)) {
        var bq = [];
        while (i < lines.length && /^\s*>\s?/.test(lines[i])) { bq.push(lines[i].replace(/^\s*>\s?/, '')); i++; }
        out.push('<blockquote class="ps-bq">' + inline(bq.join(' '), ctx) + '</blockquote>');
        continue;
      }
      if (/^\s*([-*+]|\d+\.)\s+/.test(line)) { i = parseList(lines, i, out, ctx); continue; }
      if (/^\s*<\/?(details|summary|sub|sup|div|br|kbd|b|i|em|strong|p|hr|table|thead|tbody|tr|td|th)\b/i.test(line)) {
        out.push(line); i++; continue;
      }
      if (line.trim() === '') { i++; continue; }
      var para = [line]; i++;
      while (i < lines.length && lines[i].trim() !== '' && !isBlockStart(lines[i])) { para.push(lines[i]); i++; }
      out.push('<p class="ps-p">' + inline(para.join(' '), ctx) + '</p>');
    }
    return { html: out.join('\n'), toc: toc };
  }

  window.PSMD = { render: render };
})();
