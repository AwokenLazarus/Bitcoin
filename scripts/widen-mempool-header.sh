#!/bin/bash
export PYTHONPATH=/tmp/PyMySQL-1.1.1
python3 - <<'PY'
import pymysql,time
for i in range(30):
    try:
        conn=pymysql.connect(host=__import__("os").environ.get("MEMPOOL_DB_HOST","127.0.0.1"),user=__import__("os").environ["MEMPOOL_DB_USER"],password=__import__("os").environ["MEMPOOL_DB_PASSWORD"],database="mempool",connect_timeout=3)
        break
    except Exception:
        time.sleep(2)
else:
    raise SystemExit("mariadb not up")
cur=conn.cursor()
cur.execute("SHOW COLUMNS FROM blocks LIKE 'header'")
col=cur.fetchone()
if col and "512" not in str(col[1]) and "text" not in str(col[1]).lower():
    cur.execute("ALTER TABLE blocks MODIFY header VARCHAR(512) NULL")
    conn.commit()
    print("widened", col, "-> varchar(512)")
else:
    print("header ok", col)
conn.close()
PY
