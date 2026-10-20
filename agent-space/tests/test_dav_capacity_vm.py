#!/usr/bin/env python3
"""DAV admission and review fixes against a fresh independent max_sessions=2 Host.

AGENT_SPACE_BROWSER_TEST_DIR supplies port, users.json (may be {}), admin-token
and dav-port. All users created here are deleted; never target a daily-use Host.
"""
import json
import time
import unittest
import urllib.error
import urllib.parse
import urllib.request
import uuid

from browser_client import BASE, Q, USERS, Client, text

DAV = "http://127.0.0.1:" + (Q / "dav-port").read_text().strip()
ADMIN = (Q / "admin-token").read_text().strip()
ERROR_HEADER = "Agent-Space-Error-Code"


def http(method, url, token, body=None, headers=None):
    if isinstance(body, dict):
        body = json.dumps(body).encode()
        headers = {"Content-Type": "application/json", **(headers or {})}
    req = urllib.request.Request(url, data=body, method=method,
                                 headers={"Authorization": "Bearer " + token, **(headers or {})})
    try:
        response = urllib.request.urlopen(req, timeout=90)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        return response.status, response.headers, response.read()


class DavCapacityVM(unittest.TestCase):
    def test_capacity_release_noncapacity_errors_and_review_fixes(self):
        created = []
        checks = []
        prefix = "review" + uuid.uuid4().hex[:8]

        def create(suffix, image):
            name = prefix + suffix
            code, _, body = http("POST", BASE + "/admin/v1/users", ADMIN,
                                 {"username": name, "image": image})
            self.assertEqual(code, 201)
            user = json.loads(body)
            created.append(user)
            USERS[name] = user
            code, _, body = http("POST", BASE + "/admin/v1/users/" + name + "/dav-token",
                                 ADMIN, {"agentSpaceId": user["agentSpaceId"]})
            self.assertEqual(code, 200)
            user["dav_token"] = json.loads(body)["token"]
            return user

        def enabled(user, value):
            code, _, body = http("PATCH", BASE + "/admin/v1/users/" + user["username"], ADMIN,
                                 {"agentSpaceId": user["agentSpaceId"], "enabled": value})
            self.assertIn(code, (200, 202))
            deadline = time.monotonic() + 45
            while code == 202 or json.loads(body)["stopPending"]:
                self.assertLess(time.monotonic(), deadline, "workspace stop did not finish")
                time.sleep(.2)
                code, _, body = http("GET", BASE + "/admin/v1/users/" + user["username"], ADMIN)
                self.assertEqual(code, 200)
            self.assertEqual(json.loads(body)["status"], "active" if value else "disabled")

        def dav(user, method="PROPFIND", path="", body=None, headers=None):
            return http(method, DAV + "/u/" + user["username"] + "/dav/" + path,
                        user["dav_token"], body, {"Depth": "1", **(headers or {})}
                        if method == "PROPFIND" else headers)

        def observe(user):
            client = Client(user["username"])
            client.initialize()
            value, result = client.finish(client.tool("browser_snapshot", {}))
            # Read-only cold-start observation can time out without replaying an action.
            if result.get("isError"):
                value, result = client.finish(client.tool("browser_snapshot", {}))
            self.assertFalse(result.get("isError"), value)
            self.assertEqual(value["state"], "succeeded")
            self.assertTrue(value["value"]["page_id"])
            self.assertEqual(value["control"]["mode"], "AI")

        try:
            image = "nikolaik/python-nodejs:python3.12-nodejs22-slim"
            active = create("a", image)
            occupied = create("b", image)
            waiting = create("c", {"preset": "runcove-desktop"})
            self.assertEqual(dav(active)[0], 207)
            self.assertEqual(dav(occupied)[0], 207)
            for method, path, body in [("PROPFIND", "", None), ("PUT", "not-started.txt", b"not forwarded")]:
                code, headers, response_body = dav(waiting, method, path, body)
                self.assertEqual(code, 503)
                self.assertEqual(headers["Retry-After"], "1")
                self.assertEqual(headers[ERROR_HEADER], "workspace_capacity_exhausted")
                self.assertEqual(response_body, b"")
            self.assertEqual(dav(active)[0], 207)
            checks.append("full_capacity_rejects_new_space_active_space_remains_accessible")
            enabled(occupied, False)
            code, headers, _ = dav(waiting)
            self.assertEqual(code, 207)
            self.assertIsNone(headers.get(ERROR_HEADER))
            self.assertEqual(dav(waiting, "GET", "not-started.txt")[0], 404)
            checks.append("released_slot_allows_listing_rejected_put_did_not_create_a_file")

            self.assertEqual(dav(active, "MKCOL", "review-target")[0], 201)
            self.assertEqual(dav(active, "PUT", "review-target/keep", b"preserved")[0], 201)
            destination = DAV + "/u/" + active["username"] + "/dav/review-target/"
            self.assertEqual(dav(active, "MOVE", "%2Eagent-space//",
                                 headers={"Destination": destination})[0], 403)
            self.assertEqual(dav(active, "GET", "review-target/keep")[2], b"preserved")
            checks.append("forbidden_encoded_move_preserves_destination")

            observe(waiting)
            enabled(waiting, False)
            enabled(waiting, True)
            # Disable revokes both credentials; enabling does not restore an old token.
            code, _, body = http("POST", BASE + "/admin/v1/users/" + waiting["username"] + "/token",
                                 ADMIN, {"agentSpaceId": waiting["agentSpaceId"]})
            self.assertEqual(code, 200)
            waiting["token"] = json.loads(body)["token"]
            observe(waiting)
            checks.append("desktop_recovers_after_disable_and_enable")

            enabled(active, False)
            broken = create("d", "127.0.0.1:1/qualification/missing:fixed")
            code, headers, _ = dav(broken)
            self.assertEqual(code, 503)
            self.assertIsNone(headers.get(ERROR_HEADER))
            checks.append("noncapacity_activation_failure_has_no_capacity_error_code")
        finally:
            for user in reversed(created):
                url = BASE + "/admin/v1/users/" + user["username"] + "?agentSpaceId=" + urllib.parse.quote(user["agentSpaceId"])
                code, _, _ = http("DELETE", url, ADMIN)
                self.assertIn(code, (202, 204))
                deadline = time.monotonic() + 30
                while http("GET", BASE + "/admin/v1/users/" + user["username"], ADMIN)[0] != 404:
                    self.assertLess(time.monotonic(), deadline, "test user cleanup did not finish")
                    time.sleep(.2)
                USERS.pop(user["username"], None)
        (Q / "dav-capacity-result.json").write_text(json.dumps({
            "max_sessions": 2, "passed": checks, "temporary_users_deleted": True,
        }, indent=2) + "\n")


if __name__ == "__main__":
    unittest.main(verbosity=2)
