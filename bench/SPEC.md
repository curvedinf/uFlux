# Benchmark Specification

All programs read the input file path from argv[1], output results to stdout,
and exit 0. Output format must match exactly for cross-validation.

## 1. Log Extraction (`logextract`)

**Input:** Apache combined access log (~500MB, 3.7M lines)

**Line format:**
```
IP - - [DD/Mon/YYYY:HH:MM:SS +0000] "METHOD PATH HTTP/1.1" STATUS BYTES "REF" "UA"
```

**Parsing approach:** Split line by space. Relevant tokens:
- `[0]` = IP address
- `[3]` = timestamp starting with `[DD/Mon/YYYY:HH` — split by `:`, index 3 = hour
- `[8]` = HTTP status code (integer)
- `[9]` = bytes transferred (integer)

**Compute:**
1. Total lines processed
2. Total bytes transferred (sum of bytes field)
3. HTTP status distribution: 2xx, 3xx, 4xx, 5xx counts
4. Requests per hour (24 integer counts, index 0-23)
5. Top 10 IP addresses by request count

**Output format (exact):**
```
=== LOG EXTRACTION RESULTS ===
total_lines: <int>
total_bytes: <int>
status_2xx: <int>
status_3xx: <int>
status_4xx: <int>
status_5xx: <int>
hour_00: <int>
hour_01: <int>
...
hour_23: <int>
top_ip_1: <ip> (<count>)
top_ip_2: <ip> (<count>)
...
top_ip_10: <ip> (<count>)
```

## 2. Data Analytics (`analytics`)

**Input:** Sales CSV (~500MB, 5.8M rows)

**Header:** `order_id,date,region,country,product,category,channel,units,unit_price,discount,total,reps`

**Parsing approach:** Skip first line (header). Split each row by comma. Relevant columns:
- `[2]` = region
- `[3]` = country
- `[4]` = product
- `[7]` = units (integer)
- `[10]` = total (float, 2 decimal places)

**Compute:**
1. Total rows processed
2. Total revenue (sum of total)
3. Average order value (total_revenue / rows)
4. Total units sold (sum of units)
5. Revenue by region (6 regions, sorted by revenue descending)
6. Top 5 products by revenue
7. Top 3 countries by revenue

**Output format (exact):**
```
=== DATA ANALYTICS RESULTS ===
total_rows: <int>
total_revenue: <float 2dp>
avg_order: <float 2dp>
total_units: <int>
region_1: <name> $<float 2dp>
region_2: <name> $<float 2dp>
region_3: <name> $<float 2dp>
region_4: <name> $<float 2dp>
region_5: <name> $<float 2dp>
region_6: <name> $<float 2dp>
product_1: <name> $<float 2dp>
product_2: <name> $<float 2dp>
product_3: <name> $<float 2dp>
product_4: <name> $<float 2dp>
product_5: <name> $<float 2dp>
country_1: <name> $<float 2dp>
country_2: <name> $<float 2dp>
country_3: <name> $<float 2dp>
```

## Tie-breaking

- For top-N rankings, sort by value descending. Ties broken by name ascending.
