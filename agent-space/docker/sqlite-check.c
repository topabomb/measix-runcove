/* Independent, read-only metadata validation. Never creates a database. */
#include <stdio.h>
#include <string.h>
#include <sqlite3.h>

static int scalar(sqlite3 *db, const char *sql, const char *label) {
    sqlite3_stmt *statement = NULL;
    int rc = sqlite3_prepare_v2(db, sql, -1, &statement, NULL);
    if (rc == SQLITE_OK) rc = sqlite3_step(statement);
    if (rc != SQLITE_ROW) {
        fprintf(stderr, "%s: %s (code %d)\n", label, sqlite3_errmsg(db), rc);
        sqlite3_finalize(statement);
        return 1;
    }
    printf("%s=%lld\n", label, sqlite3_column_int64(statement, 0));
    sqlite3_finalize(statement);
    return 0;
}

int main(int argc, char **argv) {
    printf("sqlite_version=%s\n", sqlite3_libversion());
    printf("sqlite_source_id=%s\n", sqlite3_sourceid());
    if (strcmp(sqlite3_libversion(), "3.51.3") != 0) return 1;
    if (argc == 1) return 0;
    if (argc != 2) return 2;
    sqlite3 *db = NULL;
    int rc = sqlite3_open_v2(argv[1], &db, SQLITE_OPEN_READONLY | SQLITE_OPEN_URI, NULL);
    if (rc != SQLITE_OK) {
        fprintf(stderr, "open: %s (code %d)\n", sqlite3_errmsg(db), rc);
        sqlite3_close(db);
        return 1;
    }
    sqlite3_busy_timeout(db, 5000);
    if (sqlite3_db_readonly(db, "main") != 1) { sqlite3_close(db); return 1; }
    printf("read_only=1\n");
    sqlite3_stmt *statement = NULL;
    rc = sqlite3_prepare_v2(db, "PRAGMA integrity_check", -1, &statement, NULL);
    int rows = 0, failures = 0;
    if (rc == SQLITE_OK) {
        while ((rc = sqlite3_step(statement)) == SQLITE_ROW) {
            const char *value = (const char *)sqlite3_column_text(statement, 0);
            printf("integrity_check=%s\n", value ? value : "NULL");
            rows++;
            if (!value || strcmp(value, "ok") != 0) failures++;
        }
    }
    if (rc != SQLITE_DONE || rows != 1) failures++;
    sqlite3_finalize(statement);
    if (failures) { sqlite3_close(db); return 1; }
    rc = sqlite3_prepare_v2(db, "PRAGMA foreign_key_check", -1, &statement, NULL);
    rows = 0;
    if (rc == SQLITE_OK) while ((rc = sqlite3_step(statement)) == SQLITE_ROW) rows++;
    printf("foreign_key_violations=%d\n", rows);
    sqlite3_finalize(statement);
    if (rc != SQLITE_DONE || rows) { sqlite3_close(db); return 1; }
    failures |= scalar(db, "SELECT count(*) FROM sandbox", "sandboxes");
    failures |= scalar(db, "SELECT count(*) FROM volume", "volumes");
    failures |= scalar(db, "SELECT count(*) FROM run", "runs");
    /* Unknown/NULL states are not proof that every VM has terminated. */
    failures |= scalar(db, "SELECT count(*) FROM run WHERE status IS NOT 'Terminated'", "active_runs");
    sqlite3_close(db);
    return failures ? 1 : 0;
}
