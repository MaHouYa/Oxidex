import logging
import os
import time

from ulauncher.api.client.EventListener import EventListener
from ulauncher.api.client.Extension import Extension
from ulauncher.api.shared.action.ActionList import ActionList
from ulauncher.api.shared.action.CopyToClipboardAction import CopyToClipboardAction
from ulauncher.api.shared.action.DoNothingAction import DoNothingAction
from ulauncher.api.shared.action.ExtensionCustomAction import ExtensionCustomAction
from ulauncher.api.shared.action.HideWindowAction import HideWindowAction
from ulauncher.api.shared.action.OpenAction import OpenAction
from ulauncher.api.shared.action.RenderResultListAction import RenderResultListAction
from ulauncher.api.shared.event import ItemEnterEvent, KeywordQueryEvent
from ulauncher.api.shared.item.ExtensionResultItem import ExtensionResultItem

from oxidex_client import OxidexClient, OxidexError, default_daemon_socket_path, start_daemon_once


logger = logging.getLogger(__name__)

ICON = "images/icon.png"
START_COOLDOWN_SECONDS = 5.0
START_WAIT_SECONDS = 1.0
MAX_RESULTS_CAP = 100


class OxidexExtension(Extension):
    def __init__(self):
        super(OxidexExtension, self).__init__()
        self._last_start_attempt = 0.0
        self.subscribe(KeywordQueryEvent, KeywordQueryEventListener())
        self.subscribe(ItemEnterEvent, ItemEnterEventListener())

    def connect_client(self):
        socket_path = preference_text(self, "socket_path") or None
        try:
            return OxidexClient.connect(socket_path)
        except Exception as first_error:
            self._maybe_start_daemon()
            deadline = time.monotonic() + START_WAIT_SECONDS
            last_error = first_error
            while time.monotonic() < deadline:
                time.sleep(0.1)
                try:
                    return OxidexClient.connect(socket_path)
                except Exception as err:
                    last_error = err
            path = socket_path or default_daemon_socket_path()
            raise OxidexError("failed to connect to oxidexd at %s: %s" % (path, last_error))

    def result_limit(self):
        value = preference_text(self, "max_results") or "25"
        try:
            parsed = int(value)
        except ValueError:
            parsed = 25
        return max(1, min(parsed, MAX_RESULTS_CAP))

    def display_mode(self):
        value = preference_text(self, "display_mode") or "device_label"
        if value == "full_path":
            return value
        return "device_label"

    def _maybe_start_daemon(self):
        now = time.monotonic()
        if now - self._last_start_attempt < START_COOLDOWN_SECONDS:
            return
        self._last_start_attempt = now
        try:
            start_daemon_once()
        except Exception:
            logger.exception("failed to start oxidexd")


class KeywordQueryEventListener(EventListener):
    def on_event(self, event, extension):
        query = (event.get_argument() or "").strip()
        try:
            if not query:
                return RenderResultListAction(home_items(extension))
            if query.startswith(":"):
                return RenderResultListAction(command_items(extension, query))
            return RenderResultListAction(search_items(extension, query))
        except Exception as err:
            logger.exception("oxidex query failed")
            return RenderResultListAction(error_items(err))


class ItemEnterEventListener(EventListener):
    def on_event(self, event, extension):
        data = event.get_data() or {}
        action = data.get("action")
        try:
            if action == "result_actions":
                return RenderResultListAction(result_action_items(data.get("row") or {}))
            if action == "open":
                return open_result_action(extension, data.get("row") or {}, data.get("mode"))
            if action == "copy_path":
                return copy_path_action(extension, data.get("row") or {})
            if action == "scan_device":
                return RenderResultListAction(scan_device_items(extension, data.get("device_id")))
            if action == "scan_menu":
                return RenderResultListAction(scan_menu_items(extension, data.get("filter") or ""))
            if action == "status":
                return RenderResultListAction(status_items(extension))
            if action == "help":
                return RenderResultListAction(help_items())
            return HideWindowAction()
        except Exception as err:
            logger.exception("oxidex item action failed")
            return RenderResultListAction(error_items(err))


def search_items(extension, query):
    client = extension.connect_client()
    try:
        result = client.search(query, extension.result_limit())
    finally:
        client.close()

    rows = result.get("rows") or []
    if not rows:
        return [
            item(
                "No Oxidex results",
                "No indexed file names matched: %s" % query,
                DoNothingAction(),
            )
        ]

    items = [search_result_item(extension, row) for row in rows]
    if result.get("truncated"):
        items.append(
            item(
                "More results available",
                "Narrow the query or increase the extension result limit.",
                DoNothingAction(),
            )
        )
    return items


def search_result_item(extension, row):
    name = row.get("name") or "(unnamed)"
    return item(
        name,
        result_description(extension, row),
        ExtensionCustomAction({"action": "result_actions", "row": row}, keep_app_open=True),
    )


def result_action_items(row):
    name = row.get("name") or "(unnamed)"
    mounted = row.get("mounted") is True
    actions = []

    if mounted:
        actions.append(
            item(
                "Open",
                "Open %s" % name,
                ExtensionCustomAction(
                    {"action": "open", "mode": "file", "row": row},
                    keep_app_open=False,
                ),
            )
        )
        actions.append(
            item(
                "Open Folder",
                "Open the containing folder",
                ExtensionCustomAction(
                    {"action": "open", "mode": "folder", "row": row},
                    keep_app_open=False,
                ),
            )
        )
    else:
        actions.append(
            item(
                "Open unavailable",
                "This indexed device is not currently mounted.",
                DoNothingAction(),
            )
        )

    actions.append(
        item(
            "Copy Path",
            "Copy the resolved path when mounted, otherwise the indexed display path",
            ExtensionCustomAction(
                {"action": "copy_path", "row": row},
                keep_app_open=False,
            ),
        )
    )
    actions.append(
        item(
            "Copy Name",
            "Copy %s" % name,
            ActionList([CopyToClipboardAction(name), HideWindowAction()]),
        )
    )
    actions.append(
        item(
            "Rescan This Device",
            device_label(row),
            ExtensionCustomAction(
                {
                    "action": "scan_device",
                    "device_id": row.get("hit", {}).get("device_id"),
                },
                keep_app_open=True,
            ),
        )
    )
    return actions


def open_result_action(extension, row, mode):
    resolved = resolve_result(extension, row)
    if not resolved.get("mounted"):
        return RenderResultListAction(
            [
                item(
                    "Open unavailable",
                    "The indexed device is not currently mounted.",
                    DoNothingAction(),
                )
            ]
        )

    path = resolved.get("path") or ""
    if mode == "folder" and not row.get("is_dir"):
        path = os.path.dirname(path) or path
    return ActionList([OpenAction(path), HideWindowAction()])


def copy_path_action(extension, row):
    path = row.get("display_path") or row.get("internal_path") or row.get("name") or ""
    try:
        resolved = resolve_result(extension, row)
        path = resolved.get("path") or path
    except Exception:
        logger.exception("failed to resolve path before copying")
    return ActionList([CopyToClipboardAction(path), HideWindowAction()])


def resolve_result(extension, row):
    hit = row.get("hit") or {}
    device_id = hit.get("device_id")
    record_idx = hit.get("record_idx")
    if not device_id or record_idx is None:
        raise OxidexError("search result is missing its Oxidex hit id")

    client = extension.connect_client()
    try:
        return client.resolve_path(device_id, record_idx)
    finally:
        client.close()


def command_items(extension, query):
    command, _, rest = query.partition(" ")
    command = command.lower()
    rest = rest.strip()

    if command in (":scan", ":rescan"):
        return scan_menu_items(extension, rest)
    if command == ":status":
        return status_items(extension)
    if command in (":help", ":?"):
        return help_items()

    return [
        item(
            "Unknown Oxidex command",
            "Use :scan, :rescan, :status, or :help.",
            ExtensionCustomAction({"action": "help"}, keep_app_open=True),
        )
    ]


def home_items(extension):
    items = [
        item("Search Oxidex", "Type file name terms after the keyword.", DoNothingAction()),
        item(
            "Rescan devices",
            "Use :scan or :rescan to choose a device.",
            ExtensionCustomAction({"action": "scan_menu", "filter": ""}, keep_app_open=True),
        ),
        item(
            "Oxidex status",
            "Show daemon, scanner, index, and scan job status.",
            ExtensionCustomAction({"action": "status"}, keep_app_open=True),
        ),
    ]
    try:
        status = fetch_status(extension)
        items.insert(0, status_summary_item(status))
    except Exception as err:
        items.insert(0, connection_error_item(err))
    return items


def help_items():
    return [
        item("ox query terms", "Search indexed file names.", DoNothingAction()),
        item("ox :scan", "Choose a known or indexed device to scan.", DoNothingAction()),
        item("ox :rescan", "Alias for :scan.", DoNothingAction()),
        item("ox :status", "Show daemon, scanner, index, and job status.", DoNothingAction()),
        item("ox :help", "Show this command list.", DoNothingAction()),
    ]


def status_items(extension):
    status, indexes, jobs = fetch_status(extension)
    items = [status_summary_item((status, indexes, jobs))]

    scanner = status.get("scanner_status") or {}
    scanner_state = "reachable" if scanner.get("reachable") else "not reachable"
    scanner_detail = scanner.get("last_error") or scanner.get("socket") or status.get("scanner_socket")
    items.append(item("Scanner daemon %s" % scanner_state, scanner_detail or "", DoNothingAction()))

    active_jobs = [job for job in jobs if job.get("state") in ("queued", "running")]
    if active_jobs:
        for job in active_jobs[:5]:
            items.append(
                item(
                    "Scan job %s: %s" % (job.get("job_id"), job.get("state")),
                    "%s - %s%% - %s"
                    % (job.get("device_id"), job.get("progress", 0), job.get("message", "")),
                    DoNothingAction(),
                )
            )
    else:
        items.append(item("No active scan jobs", "Use :scan to queue a rescan.", DoNothingAction()))

    stale_indexes = [idx for idx in indexes if idx.get("stale")]
    if stale_indexes:
        for idx in stale_indexes[:5]:
            state = idx.get("state") or {}
            items.append(
                item(
                    "Stale index: %s" % index_label(idx),
                    state.get("stale_reason") or "A rescan is recommended.",
                    ExtensionCustomAction(
                        {"action": "scan_device", "device_id": idx.get("device_id")},
                        keep_app_open=True,
                    ),
                )
            )
    else:
        items.append(item("No stale indexes reported", "Indexed devices look fresh.", DoNothingAction()))

    return items


def fetch_status(extension):
    client = extension.connect_client()
    try:
        return client.status(), client.indexes(), client.jobs()
    finally:
        client.close()


def status_summary_item(status_tuple):
    status, indexes, jobs = status_tuple
    active_count = len([job for job in jobs if job.get("state") in ("queued", "running")])
    description = "%s indexes, %s devices, %s active jobs" % (
        status.get("index_count", len(indexes)),
        status.get("device_count", 0),
        active_count,
    )
    return item("Oxidex daemon %s" % status.get("version", "reachable"), description, DoNothingAction())


def scan_menu_items(extension, filter_text):
    client = extension.connect_client()
    try:
        devices = client.devices()
        indexes = client.indexes()
    finally:
        client.close()

    entries = merged_device_entries(devices, indexes)
    if filter_text:
        needle = filter_text.lower()
        entries = [entry for entry in entries if needle in device_search_text(entry)]

    if not entries:
        return [item("No Oxidex devices found", "Open Oxidex or run oxidex-cli devices.", DoNothingAction())]

    entries.sort(key=lambda entry: (not entry.get("indexed", False), index_label(entry).lower()))
    return [scan_entry_item(entry) for entry in entries]


def scan_entry_item(entry):
    label = index_label(entry)
    verb = "Rescan" if entry.get("indexed") else "Index"
    return item(
        "%s %s" % (verb, label),
        device_description(entry),
        ExtensionCustomAction(
            {"action": "scan_device", "device_id": entry.get("device_id")},
            keep_app_open=True,
        ),
    )


def scan_device_items(extension, device_id):
    if not device_id:
        return [item("Cannot start scan", "Missing Oxidex device id.", DoNothingAction())]

    client = extension.connect_client()
    try:
        result = client.start_scan(device_id)
    finally:
        client.close()

    return [
        item(
            "Queued scan job %s" % result.get("job_id"),
            "%s is %s" % (result.get("device_id", device_id), result.get("state", "queued")),
            DoNothingAction(),
        )
    ]


def merged_device_entries(devices, indexes):
    by_id = {}
    for device in devices:
        entry = dict(device)
        entry["indexed"] = bool(device.get("indexed"))
        by_id[entry.get("device_id")] = entry
    for index in indexes:
        device_id = index.get("device_id")
        entry = by_id.get(device_id, {})
        merged = dict(index)
        merged.update(entry)
        merged["device_id"] = device_id
        merged["indexed"] = True
        merged["entry_count"] = entry.get("entry_count") or index.get("entry_count")
        by_id[device_id] = merged
    return [entry for key, entry in by_id.items() if key]


def result_description(extension, row):
    mounted = "mounted" if row.get("mounted") else "not mounted"
    if extension.display_mode() == "full_path":
        detail = row.get("display_path") or row.get("internal_path") or device_label(row)
    else:
        detail = "%s - %s" % (
            device_label(row),
            row.get("internal_path") or row.get("display_path") or "",
        )
    return "%s (%s)" % (detail, mounted)


def device_description(entry):
    mounted = "mounted" if entry.get("mounted") else "not mounted"
    indexed = "indexed" if entry.get("indexed") else "not indexed"
    count = entry.get("entry_count")
    suffix = ""
    if count is not None:
        suffix = ", %s entries" % count
    return "%s, %s%s - %s" % (
        mounted,
        indexed,
        suffix,
        entry.get("dev_node") or entry.get("device_id") or "",
    )


def device_search_text(entry):
    parts = [
        entry.get("device_id"),
        entry.get("label"),
        entry.get("dev_node"),
        entry.get("uuid"),
        entry.get("partuuid"),
        entry.get("primary_mount_point"),
    ]
    return " ".join([part for part in parts if part]).lower()


def device_label(row):
    return row.get("device_label") or row.get("hit", {}).get("device_id") or "device"


def index_label(entry):
    return entry.get("label") or entry.get("device_id") or entry.get("dev_node") or "device"


def error_items(err):
    return [connection_error_item(err)]


def connection_error_item(err):
    return item("Oxidex is not reachable", str(err), DoNothingAction())


def item(name, description, action):
    return ExtensionResultItem(icon=ICON, name=name, description=description, on_enter=action)


def preference_text(extension, key):
    value = extension.preferences.get(key, "")
    if value is None:
        return ""
    return str(value).strip()


if __name__ == "__main__":
    OxidexExtension().run()
