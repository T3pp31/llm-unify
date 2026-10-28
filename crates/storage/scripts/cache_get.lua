-- Check length before allocating bytes in the application process.
if redis.call('STRLEN', KEYS[1]) > tonumber(ARGV[1]) then
    return redis.error_reply('cache entry exceeds configured size')
end
return redis.call('GET', KEYS[1])
