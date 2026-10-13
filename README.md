# mold

SQLite package for Flame.

## Installation

Add the package and compile it:

```bash
fmp add https://github.com/sohamglx/mold
fmp install
```

---

## Quick Start

```flame
import mold
import std.json

let db = mold.open("app.db")

// Create a table
db.execute("CREATE TABLE IF NOT EXISTS users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)")

// Insert data with parameters
db.executeParams("INSERT INTO users (name, age) VALUES (?1, ?2)", [text("Alice"), integer(30)])

// Queries return JSON text; parse it to access rows and columns.
let rows = db.query("SELECT * FROM users")
let users = json.parse(rows)
println(users[0].name)

db.close()
```

---

## Opening Databases

```flame
// File database (creates if doesn't exist)
let db = mold.open("app.db")

// In-memory database
let mem = mold.openInMemory()

// Open with custom flags (path, readOnly, create)
let readOnlyDb = mold.openWithFlags("app.db", true, false)
```

---

## Executing SQL & Queries

### Execute (INSERT, UPDATE, DELETE)

```flame
// Without parameters
let changes = db.execute("DELETE FROM users WHERE age < 18")

// With positional parameters (typed array)
db.executeParams("INSERT INTO users (name, age) VALUES (?1, ?2)", [text("Bob"), integer(25)])

// Legacy JSON parameters are also accepted by executeParamsJson
db.executeParamsJson("INSERT INTO users (name, age) VALUES (:name, :age)", '{"name": "Charlie", "age": 28}')

// Execute multiple statements (schema setup, migrations)
db.executeBatch("CREATE TABLE a (id INT); CREATE TABLE b (id INT);")
```

### Query (SELECT)

`query` and `queryParams` return JSON strings containing an array of row objects. Use `std.json` to parse the result before accessing a row or column. Positional query parameters are typed arrays of `SqlParam` values.

```flame
import std.json

// Query all rows
let allRows = db.query("SELECT * FROM users")
// Output: [{"age":30,"id":1,"name":"Alice"},{"age":25,"id":2,"name":"Bob"}]
let users = json.parse(allRows)
let firstName = users[0].name
// Output: Alice

// Query with parameters
let adultUsers = db.queryParams("SELECT * FROM users WHERE age >= ?1", [integer(21)])
let adults = json.parse(adultUsers)

// Query a single row
let user = db.queryRow("SELECT * FROM users WHERE id = ?1", [integer(1)])
// Output: {"age":30,"id":1,"name":"Alice"}

// Extract just one column as a plain string
let name = db.queryString("SELECT name FROM users ORDER BY id DESC LIMIT 1", [])
// Output: Alice
```

---

## Prepared Statements

Reuse compiled statements for queries and schema inspection:

```flame
let stmt = db.prepare("SELECT name FROM users WHERE age > ?1")

// Query rows
let rows = stmt.query([integer(20)])

// Inspect statement schema
let count = stmt.columnCount()      // Number of result columns
let names = stmt.columnNames()      // Column names: ["name"]
let sql = stmt.sql()                // Original SQL query text

// Execute update with statement
let insertStmt = db.prepare("INSERT INTO users (name, age) VALUES (?1, ?2)")
insertStmt.execute([text("David"), integer(35)])
```

---

## Transactions & Savepoints

### Managed Transactions

```flame
let tx = db.transaction()

tx.execute("INSERT INTO users (name, age) VALUES ('Eve', 22)")

// Nested savepoint
tx.savepoint("step1")
tx.execute("INSERT INTO users (name, age) VALUES ('Frank', 19)")
tx.rollbackTo("step1") // rolls back Frank, keeps Eve

tx.commit()
```

### Manual Transactions

```flame
db.begin()
// or db.beginImmediate()
// or db.beginExclusive()

db.execute("UPDATE accounts SET balance = balance - 100 WHERE id = 1")
db.execute("UPDATE accounts SET balance = balance + 100 WHERE id = 2")

db.commit()
// or db.rollback()
```

---

## Utilities & Diagnostics

```flame
// Last inserted ID and changes count
let id = db.lastInsertRowid()
let changes = db.changes()
let total = db.totalChanges()

// Table introspection
let exists = db.tableExists("users")
let tables = db.tableNames()         // JSON array of table names
let info = db.tableInfo("users")     // PRAGMA table_info metadata

// Maintenance & Configuration
db.setBusyTimeout(5000)              // Wait up to 5000ms if database is locked
db.vacuum()                          // Compact and optimize database file
db.backupTo("backup.db")             // Online zero-downtime database backup

// Error handling
if db.hasError() {
    println("SQLite Error: " + db.error())
}

// Close connection
db.close()

// Check SQLite version
let version = mold.sqliteVersion()
println("SQLite version: " + version)
```
