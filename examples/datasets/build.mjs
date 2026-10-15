// Turns public datasets into node and edge CSVs for KhonsuGraph's built-in
// CSV plugin, one folder per user story:
//
//   node examples/datasets/build.mjs <story> <raw dir> <out dir>
//
// Stories and the raw files they read (see fetch.sh for where they come from):
//
//   security  enterprise-attack.json (MITRE ATT&CK STIX) and otrf/*.json
//             (OTRF Security-Datasets Sysmon recordings)
//   sre       boutique-k8s.yaml (Google Online Boutique manifests)
//   investigation  icij/*.csv (ICIJ Offshore Leaks database)
//   audit     soc-sign-bitcoinotc.csv.gz (SNAP Bitcoin OTC trust network)
//   renting   hk-mtr.json (OpenStreetMap MTR routes) and hk-listings.csv
//             (Inside Airbnb, Hong Kong)
//
// Every story writes nodes.csv (id, label, properties...) and edges.csv
// (id, source, target, type, properties...), the shape the CSV plugin maps
// with `label_column = "label"` and `type_column = "type"`. Only Node's
// standard library is used.

import fs from "node:fs";
import path from "node:path";
import readline from "node:readline";
import zlib from "node:zlib";

const [story, rawDir, outDir] = process.argv.slice(2);
if (!story || !rawDir || !outDir) {
  console.error("usage: node build.mjs <security|sre|investigation|audit|renting> <raw dir> <out dir>");
  process.exit(2);
}

// ---------------------------------------------------------------- CSV out

class Table {
  constructor(file, fixed) {
    this.file = file;
    this.fixed = fixed;
    this.rows = [];
    this.extra = new Set();
    this.ids = new Set();
  }
  add(row) {
    if (this.ids.has(row.id)) return false;
    this.ids.add(row.id);
    for (const key of Object.keys(row)) if (!this.fixed.includes(key)) this.extra.add(key);
    this.rows.push(row);
    return true;
  }
  has(id) {
    return this.ids.has(id);
  }
  write() {
    const columns = [...this.fixed, ...[...this.extra].sort()];
    const cell = (v) => {
      if (v === undefined || v === null) return "";
      const s = String(v);
      return /[",\r\n]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
    };
    const lines = [columns.join(",")];
    for (const row of this.rows) lines.push(columns.map((c) => cell(row[c])).join(","));
    fs.writeFileSync(this.file, lines.join("\n") + "\n");
    return this.rows.length;
  }
}

fs.mkdirSync(outDir, { recursive: true });
const nodes = new Table(path.join(outDir, "nodes.csv"), ["id", "label"]);
const edges = new Table(path.join(outDir, "edges.csv"), ["id", "source", "target", "type"]);
let edgeSeq = 0;
function edge(source, target, type, props = {}) {
  if (!nodes.has(source) || !nodes.has(target)) return false;
  return edges.add({ id: `e${++edgeSeq}`, source, target, type, ...props });
}

// ---------------------------------------------------------------- CSV in

/** Rows of a CSV file as objects, streamed; quoted fields may span lines. */
async function* readCsv(file) {
  const input = file.endsWith(".gz")
    ? fs.createReadStream(file).pipe(zlib.createGunzip())
    : fs.createReadStream(file);
  const lines = readline.createInterface({ input, crlfDelay: Infinity });
  let header = null;
  let pending = "";
  for await (const line of lines) {
    const text = pending ? pending + "\n" + line : line;
    if ((text.match(/"/g) || []).length % 2 === 1) {
      pending = text;
      continue;
    }
    pending = "";
    const fields = parseLine(text);
    if (!header) {
      header = fields.map((h) => h.replace(/^﻿/, ""));
      continue;
    }
    const row = {};
    header.forEach((h, i) => (row[h] = fields[i] ?? ""));
    yield row;
  }
}

function parseLine(text) {
  const out = [];
  let field = "";
  let quoted = false;
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (quoted) {
      if (c === '"' && text[i + 1] === '"') {
        field += '"';
        i++;
      } else if (c === '"') quoted = false;
      else field += c;
    } else if (c === '"') quoted = true;
    else if (c === ",") {
      out.push(field);
      field = "";
    } else field += c;
  }
  out.push(field);
  return out;
}

const short = (text, n = 240) => {
  if (!text) return undefined;
  const flat = String(text).replace(/\s+/g, " ").trim();
  return flat.length > n ? flat.slice(0, n - 1) + "…" : flat;
};

// ---------------------------------------------------------------- security

async function security() {
  const stix = JSON.parse(fs.readFileSync(path.join(rawDir, "enterprise-attack.json"), "utf8"));
  const live = (o) => !o.revoked && !o.x_mitre_deprecated;
  const extId = (o) => (o.external_references || []).find((r) => r.source_name === "mitre-attack")?.external_id;
  const byExt = new Map();
  const tactics = new Map();
  for (const o of stix.objects.filter(live)) {
    if (o.type === "x-mitre-tactic") {
      tactics.set(o.x_mitre_shortname, o.id);
      nodes.add({ id: o.id, label: "Tactic", name: o.name, external_id: extId(o), shortname: o.x_mitre_shortname });
    }
  }
  for (const o of stix.objects.filter(live)) {
    const common = { name: o.name, external_id: extId(o), description: short(o.description) };
    if (o.type === "attack-pattern") {
      nodes.add({ id: o.id, label: "Technique", ...common, subtechnique: !!o.x_mitre_is_subtechnique, platforms: (o.x_mitre_platforms || []).join(";") });
      if (extId(o)) byExt.set(extId(o), o.id);
    } else if (o.type === "intrusion-set") {
      nodes.add({ id: o.id, label: "Group", ...common, aliases: (o.aliases || []).join(";") });
    } else if (o.type === "malware" || o.type === "tool") {
      nodes.add({ id: o.id, label: "Software", ...common, kind: o.type });
    } else if (o.type === "course-of-action") {
      nodes.add({ id: o.id, label: "Mitigation", ...common });
    }
  }
  for (const o of stix.objects.filter(live)) {
    if (o.type === "attack-pattern") {
      for (const phase of o.kill_chain_phases || []) {
        const tactic = tactics.get(phase.phase_name);
        if (tactic) edge(o.id, tactic, "IN_TACTIC");
      }
    }
    if (o.type === "relationship") {
      const type = { uses: "USES", mitigates: "MITIGATES", "subtechnique-of": "SUBTECHNIQUE_OF" }[o.relationship_type];
      if (type) edge(o.source_ref, o.target_ref, type);
    }
  }

  // Sysmon recordings: hosts, users, processes and what they did, with the
  // events that match an ATT&CK technique linked to it.
  const otrf = path.join(rawDir, "otrf");
  const files = fs.existsSync(otrf) ? fs.readdirSync(otrf).filter((f) => f.endsWith(".json")) : [];
  const technique = (id) => byExt.get(id);
  const rules = [
    // [technique, test on a process-create event]
    ["T1059.001", (e) => /powershell/i.test(e.Image || "")],
    ["T1027", (e) => /\s-(e|enc|encodedcommand)\s/i.test(e.CommandLine || "")],
    ["T1021.006", (e) => /wsmprovhost\.exe$/i.test(e.Image || "")],
    ["T1003.001", (e) => /sekurlsa|logonpasswords|mimikatz/i.test(e.CommandLine || "")],
    ["T1087", (e) => /\b(net1?|whoami|quser)\.exe$/i.test(e.Image || "")],
  ];
  let events = 0;
  for (const file of files) {
    const recording = file.replace(/_\d{4}-\d{2}-\d{2}\d+\.json$/, "").replace(/\.json$/, "");
    const lines = readline.createInterface({ input: fs.createReadStream(path.join(otrf, file)), crlfDelay: Infinity });
    for await (const line of lines) {
      if (!line.trim()) continue;
      let e;
      try {
        e = JSON.parse(line);
      } catch {
        continue;
      }
      if (e.SourceName !== "Microsoft-Windows-Sysmon") continue;
      events++;
      const host = `host:${e.Hostname}`;
      nodes.add({ id: host, label: "Host", name: e.Hostname });
      const proc = (guid, image, extra = {}) => {
        if (!guid) return null;
        const id = `proc:${guid}`;
        if (!nodes.has(id)) {
          nodes.add({ id, label: "Process", name: path.win32.basename(image || "?"), image, recording, ...extra });
          edge(id, host, "RAN_ON");
        }
        return id;
      };
      if (e.EventID === 1) {
        const id = proc(e.ProcessGuid, e.Image, {
          command_line: short(e.CommandLine, 400),
          user: e.User,
          integrity: e.IntegrityLevel,
          started_at: e.UtcTime,
          pid: e.ProcessId,
        });
        const parent = proc(e.ParentProcessGuid, e.ParentImage, { command_line: short(e.ParentCommandLine, 400) });
        if (id && parent) edge(parent, id, "SPAWNED", { at: e.UtcTime });
        if (e.User) {
          const user = `user:${e.User}`;
          nodes.add({ id: user, label: "User", name: e.User });
          edge(user, id, "STARTED");
        }
        for (const [tid, test] of rules) if (technique(tid) && test(e)) edge(id, technique(tid), "INDICATES", { rule: tid });
      } else if (e.EventID === 3 && e.DestinationIp) {
        const id = proc(e.ProcessGuid, e.Image);
        const ip = `ip:${e.DestinationIp}`;
        nodes.add({ id: ip, label: "IP", name: e.DestinationIp, hostname: e.DestinationHostname || undefined });
        if (id) edges.add({ id: `net:${e.ProcessGuid}:${e.DestinationIp}:${e.DestinationPort}`, source: id, target: ip, type: "CONNECTED_TO", port: e.DestinationPort, protocol: e.Protocol }) || null;
      } else if (e.EventID === 10 && /lsass\.exe$/i.test(e.TargetImage || "")) {
        const source = proc(e.SourceProcessGUID, e.SourceImage);
        const target = proc(e.TargetProcessGUID, e.TargetImage);
        if (source && target) {
          edges.add({ id: `access:${e.SourceProcessGUID}:${e.TargetProcessGUID}:${e.GrantedAccess}`, source, target, type: "ACCESSED", granted_access: e.GrantedAccess, at: e.UtcTime });
          if (technique("T1003.001")) edge(source, technique("T1003.001"), "INDICATES", { rule: "lsass access" });
        }
      }
    }
  }
  console.log(`sysmon events read: ${events}`);
}

// ---------------------------------------------------------------- sre

function sre() {
  const yaml = fs.readFileSync(path.join(rawDir, "boutique-k8s.yaml"), "utf8");
  const docs = yaml.split(/^---\s*$/m);
  const deployments = [];
  for (const doc of docs) {
    if (!/^kind:\s*Deployment/m.test(doc)) continue;
    const name = doc.match(/^metadata:\s*\n\s+name:\s*(\S+)/m)?.[1];
    if (!name) continue;
    const image = doc.match(/image:\s*(\S+)/)?.[1];
    const port = doc.match(/containerPort:\s*(\d+)/)?.[1];
    const cpu = doc.match(/requests:\s*\n\s+cpu:\s*(\S+)/)?.[1];
    const memory = doc.match(/requests:\s*\n(?:\s+cpu:.*\n)?\s+memory:\s*(\S+)/)?.[1];
    const deps = [...doc.matchAll(/value:\s*"?([a-z0-9-]+):(\d+)"?/g)].map((m) => m[1]);
    deployments.push({ name, image, port, cpu, memory, deps });
  }
  // Which team owns which service, and the language it is written in, as the
  // Online Boutique README lists them.
  const language = {
    frontend: "Go", cartservice: "C#", productcatalogservice: "Go", currencyservice: "Node.js",
    paymentservice: "Node.js", shippingservice: "Go", emailservice: "Python", checkoutservice: "Go",
    recommendationservice: "Python", adservice: "Java", loadgenerator: "Python", "redis-cart": "Redis",
    shoppingassistantservice: "Python",
  };
  const tier = (n) => (n === "frontend" ? "edge" : n === "loadgenerator" ? "traffic" : n.startsWith("redis") ? "data" : "backend");
  for (const d of deployments) {
    nodes.add({ id: d.name, label: "Service", name: d.name, language: language[d.name], tier: tier(d.name), port: d.port, cpu_request: d.cpu, memory_request: d.memory, image: d.image });
  }
  for (const d of deployments) for (const dep of new Set(d.deps)) edge(d.name, dep, "CALLS");
}

// ---------------------------------------------------------------- investigation

async function investigation() {
  const dir = path.join(rawDir, "icij");
  // A case file, not the whole database: Panama Papers companies registered
  // through Hong Kong intermediaries, incorporated 2012-2015, and everyone
  // and every address one step from them.
  const seeds = new Set();
  for await (const r of readCsv(path.join(dir, "nodes-entities.csv"))) {
    if (r.sourceID !== "Panama Papers" || r.country_codes !== "HKG") continue;
    const year = Number((r.incorporation_date || "").slice(-4));
    if (year < 2012 || year > 2015) continue;
    if (seeds.size >= 2500) break;
    seeds.add(r.node_id);
    nodes.add({
      id: r.node_id, label: "Entity", name: r.name, jurisdiction: r.jurisdiction_description, status: r.status,
      incorporated: r.incorporation_date, inactivated: r.inactivation_date || undefined, provider: r.service_provider,
      countries: r.countries, source: r.sourceID,
    });
  }
  const links = [];
  const neighbors = new Set();
  for await (const r of readCsv(path.join(dir, "relationships.csv"))) {
    if (seeds.has(r.node_id_start) || seeds.has(r.node_id_end)) {
      links.push(r);
      neighbors.add(r.node_id_start);
      neighbors.add(r.node_id_end);
    }
  }
  const load = async (file, label, fields) => {
    for await (const r of readCsv(path.join(dir, file))) {
      if (!neighbors.has(r.node_id) || nodes.has(r.node_id)) continue;
      const row = { id: r.node_id, label, name: r.name || r.address };
      for (const [k, v] of Object.entries(fields)) if (r[v]) row[k] = r[v];
      nodes.add(row);
    }
  };
  await load("nodes-officers.csv", "Officer", { countries: "countries", source: "sourceID" });
  await load("nodes-intermediaries.csv", "Intermediary", { countries: "countries", status: "status", address: "address" });
  await load("nodes-addresses.csv", "Address", { countries: "countries", address: "address" });
  await load("nodes-entities.csv", "Entity", { jurisdiction: "jurisdiction_description", status: "status", incorporated: "incorporation_date", countries: "countries" });
  const typeOf = (t) => t.toUpperCase().replace(/[^A-Z0-9]+/g, "_").replace(/^_|_$/g, "");
  for (const r of links) edge(r.node_id_start, r.node_id_end, typeOf(r.rel_type), { link: r.link || undefined, start: r.start_date || undefined, end: r.end_date || undefined });
}

// ---------------------------------------------------------------- audit

async function audit() {
  const raw = zlib.gunzipSync(fs.readFileSync(path.join(rawDir, "soc-sign-bitcoinotc.csv.gz"))).toString("utf8");
  const ratings = raw.trim().split("\n").map((l) => l.split(",")).map(([s, t, r, time]) => ({ s, t, r: Number(r), time: Number(time) }));
  const stats = new Map();
  const of = (id) => stats.get(id) ?? stats.set(id, { given: 0, received: 0, negative: 0, total: 0, first: Infinity }).get(id);
  for (const x of ratings) {
    of(x.s).given++;
    const t = of(x.t);
    t.received++;
    t.total += x.r;
    if (x.r < 0) t.negative++;
    for (const id of [x.s, x.t]) of(id).first = Math.min(of(id).first, x.time);
  }
  for (const [id, s] of stats) {
    nodes.add({
      id: `acct:${id}`, label: "Account", name: `#${id}`, ratings_given: s.given, ratings_received: s.received,
      negative_received: s.negative, rating_sum: s.total, flagged: s.negative >= 5 && s.negative * 2 > s.received,
      first_seen: new Date(s.first * 1000).toISOString().slice(0, 10),
    });
  }
  for (const x of ratings) {
    edge(`acct:${x.s}`, `acct:${x.t}`, x.r < 0 ? "DISTRUSTS" : "TRUSTS", { rating: x.r, at: new Date(x.time * 1000).toISOString().slice(0, 10) });
  }
}

// ---------------------------------------------------------------- renting

async function renting() {
  const osm = JSON.parse(fs.readFileSync(path.join(rawDir, "hk-mtr.json"), "utf8"));
  const byId = new Map(osm.elements.filter((e) => e.type === "node").map((n) => [n.id, n]));
  const isMtr = (r) => /港鐵|MTR|機場快綫|Airport Express|東涌綫|Tung Chung|南港島綫/.test(r.tags.name || "") && !/地铁/.test(r.tags.name || "");
  const stationKey = (n) => (n.tags?.["name:en"] || n.tags?.name || "").replace(/\s*\(.*\)$/, "").trim();
  const stations = new Map();
  for (const route of osm.elements.filter((e) => e.type === "relation" && isMtr(e))) {
    const lineName = (route.tags["name:en"] || route.tags.name).replace(/\s*\(.*\)$/, "").replace(/^(MTR\s+)/, "").trim();
    const line = `line:${lineName}`;
    nodes.add({ id: line, label: "Line", name: lineName, name_zh: (route.tags["name:zh"] || route.tags.name || "").split(" ")[0], colour: route.tags.colour });
    let previous = null;
    for (const m of route.members.filter((m) => m.type === "node" && /stop/.test(m.role))) {
      const n = byId.get(m.ref);
      const key = n && stationKey(n);
      if (!key) continue;
      const id = `station:${key}`;
      if (!stations.has(id)) {
        stations.set(id, { lat: n.lat, lon: n.lon });
        nodes.add({ id, label: "Station", name: key, name_zh: n.tags["name:zh"] || n.tags.name, lat: n.lat, lng: n.lon });
      }
      edges.add({ id: `on:${id}:${line}`, source: id, target: line, type: "ON_LINE" });
      if (previous && previous !== id) {
        const [a, b] = [previous, id].sort();
        edges.add({ id: `next:${a}:${b}:${line}`, source: a, target: b, type: "NEXT_TO", line: lineName });
      }
      previous = id;
    }
  }
  const metres = (a, b) => {
    const r = 6371000, rad = Math.PI / 180;
    const dLat = (b.lat - a.lat) * rad, dLon = (b.lon - a.lon) * rad;
    const h = Math.sin(dLat / 2) ** 2 + Math.cos(a.lat * rad) * Math.cos(b.lat * rad) * Math.sin(dLon / 2) ** 2;
    return Math.round(2 * r * Math.asin(Math.sqrt(h)));
  };
  let listings = 0;
  for await (const r of readCsv(path.join(rawDir, "hk-listings.csv"))) {
    const price = Number(r.price);
    if (!price) continue;
    const at = { lat: Number(r.latitude), lon: Number(r.longitude) };
    const id = `listing:${r.id}`;
    const area = `district:${r.neighbourhood}`;
    nodes.add({ id: area, label: "District", name: r.neighbourhood });
    nodes.add({ id, label: "Listing", name: short(r.name, 80), price_hkd: price, room_type: r.room_type, min_nights: Number(r.minimum_nights), reviews: Number(r.number_of_reviews), lat: at.lat, lng: at.lon, district: r.neighbourhood });
    edge(id, area, "IN_DISTRICT");
    const near = [...stations].map(([sid, s]) => [sid, metres(at, s)]).filter(([, d]) => d <= 800).sort((a, b) => a[1] - b[1]).slice(0, 3);
    for (const [sid, d] of near) edge(id, sid, "NEAR", { metres: d, walk_minutes: Math.ceil(d / 80) });
    listings++;
  }
  console.log(`listings with a price: ${listings}`);
}

const stories = { security, sre, investigation, audit, renting };
if (!stories[story]) {
  console.error(`unknown story ${story}; one of ${Object.keys(stories).join(", ")}`);
  process.exit(2);
}
await stories[story]();
console.log(`${story}: ${nodes.write()} nodes, ${edges.write()} edges -> ${outDir}`);
