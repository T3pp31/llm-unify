-- KEYS[1]: idempotency receipt; remaining keys: fixed-window counters.
-- ARGV: request hash, receipt TTL, then (limit, amount, window_ms) per counter.
-- Validate every counter before the first write: Redis scripts do not roll back
-- writes preceding a runtime error.
local receipt = redis.call('GET', KEYS[1])
if receipt then
    if receipt == ARGV[1] then return {1, 0} end
    return {2, 0}
end
local values = {}
local expiries = {}
local retry = 0
for i = 2, #KEYS do
    local offset = 3 + (i - 2) * 3
    local limit = tonumber(ARGV[offset])
    local amount = tonumber(ARGV[offset + 1])
    local raw = redis.call('GET', KEYS[i])
    local current = 0
    if raw then
        current = tonumber(raw)
        if not string.match(raw, '^%d+$') or not current or current < 0 or current > 9007199254740991 or current ~= math.floor(current) or string.format('%.0f', current) ~= raw then
            return redis.error_reply('invalid shared rate counter')
        end
        expiries[i] = redis.call('PTTL', KEYS[i])
        if expiries[i] < 0 then return redis.error_reply('rate counter has no expiry') end
    end
    values[i] = current
    if current > limit - amount then
        retry = math.max(retry, expiries[i] or tonumber(ARGV[offset + 2]), 1)
    end
end
if retry > 0 then return {0, retry} end
for i = 2, #KEYS do
    local offset = 3 + (i - 2) * 3
    redis.call('INCRBY', KEYS[i], ARGV[offset + 1])
    if values[i] == 0 and not expiries[i] then
        redis.call('PEXPIRE', KEYS[i], ARGV[offset + 2])
    end
end
redis.call('SET', KEYS[1], ARGV[1], 'PX', ARGV[2])
return {1, 0}
