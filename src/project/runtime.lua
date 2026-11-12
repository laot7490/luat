local type = type
local error = error
local tostring = tostring
local xpcall = xpcall
local traceback = debug.traceback
local format = string.format

local factories = {}
local states = {}
local values = {}

if __luat ~= nil then
    error('LuaT runtime namespace "__luat" is already in use', 0)
end

local function validateId(id)
    if type(id) ~= 'string' or id == '' then
        error('LuaT module id must be a non-empty string', 3)
    end
end

local function register(id, factory)
    validateId(id)

    if type(factory) ~= 'function' then
        error(format('LuaT module "%s" requires a factory function', id), 2)
    end

    if factories[id] ~= nil then
        error(format('LuaT module "%s" is already registered', id), 2)
    end

    factories[id] = factory
end

local function import(id)
    validateId(id)

    local state = states[id]

    if state == 'loaded' then
        return values[id]
    end

    if state == 'loading' then
        error(format('LuaT module "%s" is already loading', id), 2)
    end

    local factory = factories[id]

    if factory == nil then
        error(format('LuaT module "%s" is not registered', id), 2)
    end

    states[id] = 'loading'

    -- Only the first module result is retained, including nil.
    local ok, result = xpcall(factory, traceback)

    if not ok then
        states[id] = nil
        error(format('LuaT module "%s" failed:\n%s', id, tostring(result)), 2)
    end

    values[id] = result
    states[id] = 'loaded'

    return result
end

__luat = {
    register = register,
    import = import,
}
