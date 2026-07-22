/* Send to Grok Research — always-on via local HTTP drop (preferred). */
const DROP_URL = "http://127.0.0.1:18765/send";
const NATIVE_HOST = "dev.theesfeld.research_send";

chrome.runtime.onInstalled.addListener(() => {
  ensureMenus();
});

chrome.runtime.onStartup?.addListener?.(() => {
  ensureMenus();
});

function ensureMenus() {
  chrome.contextMenus.removeAll(() => {
    chrome.contextMenus.create({
      id: "send-selection",
      title: "Send selection to Grok Research",
      contexts: ["selection"],
    });
    chrome.contextMenus.create({
      id: "send-page",
      title: "Send page to Grok Research",
      contexts: ["page", "frame"],
    });
    chrome.contextMenus.create({
      id: "send-link",
      title: "Send link to Grok Research",
      contexts: ["link"],
    });
    chrome.contextMenus.create({
      id: "send-image",
      title: "Send image to Grok Research",
      contexts: ["image"],
    });
  });
}

chrome.contextMenus.onClicked.addListener(async (info, tab) => {
  try {
    if (info.menuItemId === "send-selection") {
      await sendPayload({
        type: "send",
        title: tab?.title || "Selection",
        url: info.pageUrl || tab?.url || "",
        selection: info.selectionText || "",
        content_type: "selection",
        captured_at: new Date().toISOString(),
      });
    } else if (info.menuItemId === "send-page") {
      await sendPage(tab);
    } else if (info.menuItemId === "send-link") {
      await sendPayload({
        type: "send",
        title: info.linkUrl || "Link",
        url: info.linkUrl || "",
        selection: info.linkUrl || "",
        content_type: "link",
        captured_at: new Date().toISOString(),
      });
    } else if (info.menuItemId === "send-image") {
      await sendPayload({
        type: "send",
        title: tab?.title || "Image",
        url: info.pageUrl || tab?.url || "",
        image_url: info.srcUrl || "",
        content_type: "image",
        captured_at: new Date().toISOString(),
      });
    }
    notify("Sent to Grok Research", "Queued for background processing.");
  } catch (e) {
    console.error(e);
    notify("Send failed", String(e.message || e));
  }
});

chrome.commands.onCommand.addListener(async (command) => {
  if (command !== "send-selection-or-page") return;
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  if (!tab) return;
  try {
    const [{ result }] = await chrome.scripting.executeScript({
      target: { tabId: tab.id },
      func: () => window.getSelection()?.toString() || "",
    });
    if (result && result.trim()) {
      await sendPayload({
        type: "send",
        title: tab.title || "Selection",
        url: tab.url || "",
        selection: result,
        content_type: "selection",
        captured_at: new Date().toISOString(),
      });
    } else {
      await sendPage(tab);
    }
    notify("Sent to Grok Research", "Queued for background processing.");
  } catch (e) {
    try {
      await sendPage(tab);
      notify("Sent to Grok Research", "Queued for background processing.");
    } catch (e2) {
      notify("Send failed", String(e2.message || e2));
    }
  }
});

async function sendPage(tab) {
  if (!tab?.id) return;
  let page_text = "";
  let page_markdown = "";
  try {
    const [{ result }] = await chrome.scripting.executeScript({
      target: { tabId: tab.id },
      func: () => {
        const title = document.title || "";
        const body = document.body ? document.body.innerText : "";
        return { title, body };
      },
    });
    page_text = result?.body || "";
  } catch (_) {
    /* restricted page */
  }
  await sendPayload({
    type: "send",
    title: tab.title || "Page",
    url: tab.url || "",
    page_text,
    page_markdown,
    content_type: "page",
    captured_at: new Date().toISOString(),
  });
}

/** Prefer localhost HTTP drop (always-on daemon). Fall back to native messaging. */
async function sendPayload(payload) {
  try {
    return await sendHttp(payload);
  } catch (httpErr) {
    console.warn("HTTP drop failed, trying native host:", httpErr);
    return await sendNative(payload);
  }
}

async function sendHttp(payload) {
  const res = await fetch(DROP_URL, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(payload),
  });
  const data = await res.json().catch(() => ({}));
  if (!res.ok || !data.ok) {
    throw new Error(data.error || `HTTP ${res.status}`);
  }
  return data;
}

function sendNative(payload) {
  return new Promise((resolve, reject) => {
    let port;
    try {
      port = chrome.runtime.connectNative(NATIVE_HOST);
    } catch (e) {
      reject(e);
      return;
    }
    const timer = setTimeout(() => {
      try {
        port.disconnect();
      } catch (_) {}
      reject(new Error("native host timeout — run: research-ingest enable"));
    }, 8000);
    port.onMessage.addListener((msg) => {
      clearTimeout(timer);
      if (msg && msg.ok) resolve(msg);
      else reject(new Error((msg && msg.error) || "native host error"));
      try {
        port.disconnect();
      } catch (_) {}
    });
    port.onDisconnect.addListener(() => {
      clearTimeout(timer);
      const err = chrome.runtime.lastError;
      if (err) reject(new Error(err.message));
    });
    port.postMessage(payload);
  });
}

function notify(title, message) {
  if (!chrome.notifications) return;
  try {
    chrome.notifications.create({
      type: "basic",
      iconUrl: "icons/icon48.png",
      title,
      message,
    });
  } catch (_) {}
}

chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
  if (msg?.type === "send-active") {
    chrome.tabs.query({ active: true, currentWindow: true }).then(async ([tab]) => {
      try {
        await sendPage(tab);
        sendResponse({ ok: true });
      } catch (e) {
        sendResponse({ ok: false, error: String(e) });
      }
    });
    return true;
  }
  if (msg?.type === "health") {
    fetch("http://127.0.0.1:18765/health")
      .then((r) => r.json())
      .then((j) => sendResponse({ ok: true, health: j }))
      .catch((e) => sendResponse({ ok: false, error: String(e) }));
    return true;
  }
  return false;
});
