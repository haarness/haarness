import type { IncomingMessage, ServerResponse } from "node:http";
const styles = `body{font:16px system-ui;background:#f2f5ff;color:#152039;margin:0;padding:28px}main{max-width:800px}h1{font-size:30px}label{display:block;margin:14px 0}input,select,textarea,button{font:inherit;padding:8px}button,a{cursor:pointer}button{margin:5px}table{border-collapse:collapse}td,th{padding:12px;border:1px solid #bcc9de}.box{padding:24px;margin:14px 0;background:white;border:1px solid #adb6cc}.card{padding:25px;background:#e4dcff;margin:12px}#tip{display:none}#help:hover+#tip{display:block}`;
const shell = (title: string, body: string, script = "") =>
  `<!doctype html><html lang="en"><head><title>${title}</title><meta name="viewport" content="width=device-width"><meta name="description" content="Local browser testing lab"><style>${styles}</style></head><body><main><h1>${title}</h1>${body}</main><script>${script}</script></body></html>`;
const input = (label: string, name: string, extra = "") =>
  `<label>${label}<input name="${name}" id="${name}" ${extra}></label>`;
export function browserLab(request: IncomingMessage, response: ServerResponse) {
  const url = new URL(request.url ?? "/", "http://" + request.headers.host);
  if (url.pathname === "/report.csv") {
    response.writeHead(200, {
      "Content-Type": "text/csv",
      "Content-Disposition": 'attachment; filename="stock.csv"',
    });
    return response.end("product,stock\nOrchid mug,17\nCedar lamp,4\n");
  }
  if (url.pathname === "/api/save") {
    response.writeHead(422, { "Content-Type": "application/json" });
    return response.end(
      JSON.stringify({
        error: "WAREHOUSE_LOCKED",
        message: "Warehouse maintenance until 18:45",
        requestId: "lab-req-742",
      }),
    );
  }
  const page = (title: string, body: string, script = "") => {
    response.setHeader("Content-Type", "text/html; charset=utf-8");
    response.end(shell(title, body, script));
  };
  switch (url.pathname) {
    case "/catalog":
      return page(
        "Willow shop",
        `<p>Free shipping on orders of $50 or more. Returns within 21 days.</p><label>Category<select id="category"><option>All</option><option>Kitchen</option><option>Lighting</option></select></label><table><thead><tr><th>Product</th><th>Category</th><th>Price</th><th>Stock</th></tr></thead><tbody id="products"></tbody></table><a href="/shipping">Shipping details</a>`,
        `const rows=[['Orchid mug','Kitchen',18,17],['Cedar lamp','Lighting',42,4],['Amber bowl','Kitchen',24,9]];function render(){products.innerHTML=rows.filter(r=>category.value==='All'||r[1]===category.value).map(r=>'<tr><td>'+r[0]+'</td><td>'+r[1]+'</td><td>$'+r[2]+'</td><td>'+r[3]+'</td></tr>').join('')}category.onchange=render;render();`,
      );
    case "/shipping":
      return page(
        "Shipping policy",
        "<p>Standard: $6, 4 business days. Express: $12, 2 business days. Free standard shipping at $50. Return window: 21 days.</p><a href='/catalog'>Back to shop</a>",
      );
    case "/compare":
      return page(
        "River shop",
        "<p>Orchid mug costs $21. Shipping is $3. Delivery takes 3 business days. Returns within 30 days.</p>",
      );
    case "/booking":
      return page(
        "Book a table",
        `<form id="booking">${input("Name", "guest", "required")}${input("Email", "email", 'type="email" required')}<label>Party size<select id="party" name="party"><option>1</option><option>2</option><option>4</option><option>6</option></select></label>${input("Date", "date", 'type="date" required')}<label>Notes<textarea name="notes" id="notes"></textarea></label><label><input type="checkbox" id="outdoor" name="outdoor"> Outdoor seating</label><button>Reserve table</button></form><p id="status" role="status">No reservation yet</p>`,
        `window.saved=null;window.submissions=0;booking.onsubmit=e=>{e.preventDefault();saved=Object.fromEntries(new FormData(booking));saved.outdoor=outdoor.checked;submissions++;status.textContent='Reservation confirmed for '+saved.guest+' — '+saved.party+' guests on '+saved.date;};`.replace(
          "status.textContent",
          "document.getElementById('status').textContent",
        ),
      );
    case "/search":
      return page(
        "Library search",
        `<form id="search">${input("Search books", "query")}<button>Search</button></form><section id="results" aria-live="polite">Enter a search term.</section>`,
        `search.onsubmit=e=>{e.preventDefault();results.textContent=query.value.toLowerCase().includes('forest')?'Forest Atlas — Mira Lane — Shelf C7':'No matching books';};`,
      );
    case "/async":
      return page(
        "Export center",
        `<button id="start">Prepare export</button><p id="status" role="status">Ready</p><button id="claim" disabled>Get export code</button><p id="code"></p>`,
        `start.onclick=()=>{document.getElementById('status').textContent='Preparing…';setTimeout(()=>{document.getElementById('status').textContent='Export ready';claim.disabled=false},1800)};claim.onclick=()=>code.textContent='Export code: FIR-629';`,
      );
    case "/embedded":
      return page(
        "Customer portal",
        `<p>Update your delivery instructions in the embedded form.</p><iframe title="Delivery details" style="width:95%;height:390px" src="http://localhost:${url.port}/frame"></iframe>`,
      );
    case "/frame":
      return page(
        "Delivery details",
        `<form id="delivery">${input("Recipient", "recipient")}${input("Delivery instruction", "instruction")}<button>Save delivery</button></form><p id="result"></p>`,
        `delivery.onsubmit=e=>{e.preventDefault();window.saved={recipient:recipient.value,instruction:instruction.value};result.textContent='Delivery saved for '+recipient.value+': '+instruction.value};`,
      );
    case "/popup":
      return page(
        "Help center",
        `<button id="help">Open return policy</button><p>Policy opens in a separate tab.</p>`,
        `help.onclick=()=>window.open('/shipping','_blank');`,
      );
    case "/dialogs":
      return page(
        "Draft manager",
        `<p id="status">Draft: Summer menu</p><button id="remove">Delete draft</button><button id="rename">Rename draft</button>`,
        `window.deleted=false;remove.onclick=()=>{if(confirm('Delete Summer menu?')){window.deleted=true;document.getElementById('status').textContent='Draft deleted'}};rename.onclick=()=>{const name=prompt('New draft name','Summer menu');if(name!==null)document.getElementById('status').textContent='Draft: '+name};`,
      );
    case "/visual":
      return page(
        "Color dashboard",
        `<canvas id="art" width="600" height="330" style="max-width:100%"></canvas>`,
        `const c=art.getContext('2d');c.fillStyle='#7036a8';c.fillRect(0,0,600,330);c.fillStyle='white';c.font='bold 64px sans-serif';c.fillText('ORCHID',40,105);c.font='120px sans-serif';c.fillText('7',250,260);`,
      );
    case "/import":
      return page(
        "Inventory import",
        `<label>Import CSV<input type="file" id="upload" accept=".csv"></label><div id="drop" role="region" aria-label="Drop CSV here" class="box">Drop CSV here</div><p id="result" role="status">Nothing imported</p>`,
        `async function read(file){window.imported=await file.text();result.textContent='Imported '+file.name+': '+window.imported.trim().split(String.fromCharCode(10)).slice(1).length+' rows. '+window.imported;}upload.onchange=()=>read(upload.files[0]);drop.ondragover=e=>e.preventDefault();drop.ondrop=e=>{e.preventDefault();window.dropTrusted=e.isTrusted;read(e.dataTransfer.files[0])};`,
      );
    case "/download":
      return page(
        "Stock reports",
        `<a href="/report.csv" download="stock.csv">Download stock report</a>`,
      );
    case "/broken":
      return page(
        "Settings preview",
        `<button id="save">Save settings</button><p id="result">Ready</p>`,
        `save.onclick=async()=>{console.error('SAVE_REJECTED: inventory writes are paused');const response=await fetch('/api/save',{method:'POST'});window.lastStatus=response.status;result.textContent='Save failed. Check diagnostics.'};`,
      );
    case "/slow":
      return page(
        "Render benchmark",
        `<button id="compute">Calculate report</button><p id="result">Not calculated</p>`,
        `compute.onclick=()=>{function expensiveReport(){const end=performance.now()+350;let n=0;while(performance.now()<end){n+=Math.sqrt(Math.random())};return n}window.lastTotal=expensiveReport();result.textContent='Report calculated';};`,
      );
    case "/audit":
      return page(
        "Accessibility sample",
        `<p>This deliberately imperfect sample is for an accessibility audit.</p><button onclick="this.textContent='Saved'"></button><img src="data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='50' height='50'%3E%3Crect width='50' height='50' fill='red'/%3E%3C/svg%3E"><input><p style="color:#dedede;background:white">Low contrast text</p>`,
      );
    case "/board":
      return page(
        "Task board",
        `<div id="todo" class="box"><h2>To do</h2><div class="card" id="card" draggable="true" role="button" aria-label="Ship browser tests">Ship browser tests</div></div><div id="done" class="box" role="region" aria-label="Done column"><h2>Done</h2></div><p id="result">Card is in To do</p>`,
        `card.ondragstart=e=>e.dataTransfer.setData('text/plain','card');done.ondragover=e=>e.preventDefault();done.ondrop=e=>{e.preventDefault();window.dropTrusted=e.isTrusted;done.appendChild(card);result.textContent='Ship browser tests moved to Done';};`,
      );
    case "/hover":
      return page(
        "Delivery estimate",
        `<button id="help" aria-label="Shipping information">ⓘ Shipping information</button><p id="tip" role="tooltip">Express cutoff: 16:30 Tokyo time</p>`,
      );
    case "/scroll":
      return page(
        "Long checklist",
        `<p>Read the final status at the bottom.</p>${Array.from({ length: 40 }, (_, i) => `<p style="margin:35px 0">Checklist item ${i + 1}</p>`).join("")}<button id="finish">Finish checklist</button><p id="result">Incomplete</p>`,
        `finish.onclick=()=>result.textContent='Checklist complete: PINE-804';`,
      );
    case "/counter":
      return page(
        "Counter",
        `<button id="increment">Add one</button><p id="result">Count: 0</p>`,
        `window.count=0;increment.onclick=()=>{count++;result.textContent='Count: '+count};`,
      );
    default:
      return page(
        "Browser lab",
        `<p>Welcome to the local test site.</p><a href="/catalog">Browse Willow shop</a><a href="/booking">Book a table</a>`,
      );
  }
}
