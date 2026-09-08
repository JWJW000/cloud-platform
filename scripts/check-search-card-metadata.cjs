// Run with node scripts/check-search-card-metadata.cjs after installing admin-web dependencies.
const {readFileSync} = require('node:fs');
const {strict: assert} = require('node:assert');
const {JSDOM} = require('../admin-web/node_modules/jsdom');
const source = readFileSync(`${__dirname}/../crates/automation-core/src/site.rs`, 'utf8');
const script = source.match(/pub const CARD_SCRAPE_SCRIPT: &str = r#"([\s\S]*?)"#;/)[1];
const dom = new JSDOM('<body>没有找到相关图书，非常相似</body>', {runScripts: 'outside-only'});
const document = dom.window.document;
function card(download) {
  const element = document.createElement('z-bookcard');
  element.setAttribute('download', download);
  document.body.append(element);
  return element;
}
const slotted = card('/dl/slotted');
slotted.innerHTML = '<span slot="title">Digital Twins</span><span slot="author">Alice Smith</span><span slot="publisher">Example Press</span>';
slotted.attachShadow({mode: 'open'}).innerHTML = '<slot name="title"></slot><slot name="author"></slot>';
const shadow = card('/dl/shadow');
shadow.attachShadow({mode: 'open'}).innerHTML = '<h3>Algorithms</h3><span class="author">Bob</span><span class="publisher">Test Press</span>';
const attrs = card('/dl/attrs');
for (const [name, value] of Object.entries({title:'Physics',author:'Carol',publisher:'Science Press',isbn:'0262033844'})) attrs.setAttribute(name,value);
const hidden = card('/dl/hidden'); hidden.hidden = true;
const rows = JSON.parse(JSON.stringify(dom.window.eval(script)));
assert.equal(rows.length, 3);
assert.deepEqual(rows[0], {title:'Digital Twins',download:'/dl/slotted',isbn:'',author:'Alice Smith',publisher:'Example Press'});
assert.equal(rows[1].author, 'Bob'); assert.equal(rows[1].publisher, 'Test Press');
assert.equal(rows[2].title, 'Physics'); assert.equal(rows[2].isbn, '0262033844');
console.log('Search card metadata: attributes, light/shadow DOM and no-result hints passed');
