(function () {
  if (window.__lazChatLoaded) return;
  window.__lazChatLoaded = true;

  var endpoint =
    window.LAZ_CHAT_URL ||
    "https://pool.awokenlazarus.xyz/api/laz/chat";
  var source = window.LAZ_CHAT_SOURCE || "web";

  function el(tag, attrs, kids) {
    var n = document.createElement(tag);
    if (attrs) Object.keys(attrs).forEach(function (k) {
      if (k === "text") n.textContent = attrs[k];
      else if (k === "html") n.innerHTML = attrs[k];
      else n.setAttribute(k, attrs[k]);
    });
    (kids || []).forEach(function (c) { n.appendChild(c); });
    return n;
  }

  var root = el("div", { id: "laz-chat-root" });
  var fab = el("button", { id: "laz-chat-fab", type: "button", text: "Ask Laz" });
  var panel = el("div", { id: "laz-chat-panel" });
  var head = el("div", { id: "laz-chat-head" }, [
    el("h2", { text: "Laz" }),
    el("p", { text: "This chain is BLAKE2b (fork 961640), not SHA-256d mainnet." }),
  ]);
  var log = el("div", { id: "laz-chat-log" });
  var input = el("input", {
    type: "text",
    maxlength: "800",
    placeholder: "BLAKE2b tip, pool hashrate, a txid…",
    "aria-label": "Ask Laz",
  });
  var send = el("button", { type: "submit", text: "Send" });
  var form = el("form", { id: "laz-chat-form" }, [input, send]);
  panel.appendChild(head);
  panel.appendChild(log);
  panel.appendChild(form);
  root.appendChild(fab);
  root.appendChild(panel);
  document.body.appendChild(root);

  function add(role, text, extra) {
    var m = el("div", { class: "laz-msg " + role + (extra || ""), text: text });
    log.appendChild(m);
    log.scrollTop = log.scrollHeight;
    return m;
  }

  add("bot", "Ask about this BLAKE2b node, Lazarus pool, miners, or Electrum. Live reads only. For wallets or miner setup use the pool Connect page and GitHub — do not trust this chat with keys or anything that could lose coins.");

  fab.addEventListener("click", function () {
    panel.classList.toggle("open");
    if (panel.classList.contains("open")) input.focus();
  });

  form.addEventListener("submit", function (ev) {
    ev.preventDefault();
    var q = (input.value || "").trim();
    if (!q) return;
    input.value = "";
    add("user", q);
    send.disabled = true;
    var wait = add("bot", "Checking the stack…");
    fetch(endpoint, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ q: q, source: source }),
    })
      .then(function (r) { return r.json().then(function (j) { return { ok: r.ok, j: j }; }); })
      .then(function (res) {
        var j = res.j || {};
        if (j.answer) wait.textContent = j.answer;
        else wait.textContent = j.error || "No reply.";
        if (!res.ok || j.error) wait.className = "laz-msg bot err";
      })
      .catch(function (e) {
        wait.textContent = "Could not reach Laz: " + e;
        wait.className = "laz-msg bot err";
      })
      .finally(function () { send.disabled = false; input.focus(); });
  });
})();
