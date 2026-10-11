// /naughtylist/: a filter box and sortable columns over tables the server already rendered.
// The page is complete without this file.
(() => {
  const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];

  // Sort a table by a column whose heading carries data-sort ("num" reads each cell's data-v).
  for (const th of $$("table.list th[data-sort]")) {
    th.tabIndex = 0;
    th.setAttribute("role", "columnheader");
    const sort = () => {
      const table = th.closest("table"), body = table.tBodies[0];
      const col = [...th.parentNode.children].indexOf(th);
      const down = th.getAttribute("aria-sort") !== "descending";
      for (const other of $$("th[aria-sort]", table)) other.removeAttribute("aria-sort");
      th.setAttribute("aria-sort", down ? "descending" : "ascending");
      const num = th.dataset.sort === "num";
      const key = (tr) => {
        const td = tr.children[col];
        if (!td) return num ? 0 : "";
        return num ? Number(td.dataset.v != null ? td.dataset.v : td.textContent.replace(/[^0-9.-]/g, "")) || 0 : td.textContent.trim().toLowerCase();
      };
      const rows = [...body.rows].map((tr) => [key(tr), tr]);
      rows.sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0) * (down ? -1 : 1));
      body.append(...rows.map((r) => r[1]));
    };
    th.addEventListener("click", sort);
    th.addEventListener("keydown", (e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); sort(); } });
  }

  // Filter every address row on the page by a part of the address.
  const box = document.getElementById("filter");
  const wrap = box && box.closest(".filter");
  const rows = $$("td.addr").map((td) => [td.textContent.toLowerCase(), td.parentNode]);
  if (!box || !wrap || rows.length < 12) return;
  wrap.hidden = false;
  const count = document.getElementById("filter-count");
  const groups = $$("details.group");
  const wasOpen = new Map(groups.map((g) => [g, g.open]));
  let timer = 0;
  const apply = () => {
    const needle = box.value.trim().toLowerCase();
    let shown = 0;
    for (const [text, tr] of rows) {
      const hit = !needle || text.includes(needle);
      tr.hidden = !hit;
      if (hit) shown++;
    }
    for (const g of groups) {
      const any = $$("tbody tr", g).some((tr) => !tr.hidden);
      g.hidden = !!needle && !any;
      g.open = needle ? any : wasOpen.get(g);
    }
    if (count) count.textContent = needle ? `${shown.toLocaleString("en-US")} of ${rows.length.toLocaleString("en-US")} rows` : "";
  };
  box.addEventListener("input", () => { clearTimeout(timer); timer = setTimeout(apply, 80); });
})();
