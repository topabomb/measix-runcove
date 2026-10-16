#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <fcntl.h>
#include <sys/stat.h>
#include <sqlite3.h>

static void query(sqlite3 *db, const char *sql) {
    char *error = NULL;
    int rc = sqlite3_exec(db, sql, NULL, NULL, &error);
    printf("QUERY code=%d extended=%d message=%s\n", rc, sqlite3_extended_errcode(db), error ? error : "ok");
    sqlite3_free(error);
    fflush(stdout);
}

int main(int argc, char **argv) {
    if (argc < 4) return 2;
    sqlite3 *db = NULL;
    setbuf(stdout, NULL);
    if (sqlite3_open(argv[2], &db) != SQLITE_OK) return 3;
    sqlite3_busy_timeout(db, 1000);
    if (!strcmp(argv[1], "writer")) {
        query(db, !strcmp(argv[3], "hold") ? "CREATE TABLE extra(payload TEXT)" : "INSERT INTO probe(payload) VALUES('writer')");
        if (!strcmp(argv[3], "hold")) { printf("READY\n"); getchar(); }
        sqlite3_close(db);
        printf("CLOSED\n");
        return 0;
    }
    query(db, "PRAGMA journal_mode=WAL; CREATE TABLE probe(id INTEGER PRIMARY KEY,payload TEXT); INSERT INTO probe(payload) VALUES('initial')");
    int pin = open(argv[2], O_PATH | O_CLOEXEC);
    if (pin < 0) return 4;
    printf("READY sqlite=%s\n", sqlite3_libversion());
    char command[64];
    while (fgets(command, sizeof(command), stdin)) {
        if (!strncmp(command, "verify", 6)) {
            if (!strcmp(argv[3], "rogue")) {
                int fd = open(argv[2], O_RDONLY | O_CLOEXEC);
                if (fd < 0) return 5;
                close(fd);
            } else {
                struct stat original, current;
                if (fstat(pin, &original) || stat(argv[2], &current)) return 6;
                if (original.st_dev != current.st_dev || original.st_ino != current.st_ino) return 7;
            }
            printf("VERIFIED\n");
        } else if (!strncmp(command, "post-stop", 9)) {
            query(db, "UPDATE probe SET payload='host-after-runtime-stop'");
            printf("UPDATED\n");
        } else if (!strncmp(command, "new-reader", 10)) {
            sqlite3 *reader = NULL;
            int rc = sqlite3_open_v2(argv[2], &reader, SQLITE_OPEN_READONLY, NULL);
            printf("OPEN code=%d\n", rc);
            query(reader, "SELECT * FROM probe");
            sqlite3_close(reader);
            printf("DONE\n");
        } else if (!strncmp(command, "exit", 4)) break;
    }
    sqlite3_close(db);
    close(pin);
    return 0;
}
