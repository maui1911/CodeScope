namespace NoScope.CodeScope.Core;

/// <summary>
/// Minimal result type for expected, non-exceptional failures
/// (git returning nonzero, missing config file, etc.).
/// Exceptions are reserved for programmer errors and truly unexpected conditions.
/// </summary>
public readonly record struct Result<T>
{
    private readonly T? _value;
    private readonly string? _error;

    private Result(T? value, string? error, bool isSuccess)
    {
        _value = value;
        _error = error;
        IsSuccess = isSuccess;
    }

    public bool IsSuccess { get; }

    public bool IsFailure => !IsSuccess;

    public T Value => IsSuccess
        ? _value!
        : throw new InvalidOperationException($"Result is a failure: {_error}");

    public string Error => _error ?? string.Empty;

    public static Result<T> Ok(T value) => new(value, null, true);

    public static Result<T> Fail(string error) => new(default, error, false);

    public Result<TOut> Map<TOut>(Func<T, TOut> mapper) =>
        IsSuccess ? Result<TOut>.Ok(mapper(_value!)) : Result<TOut>.Fail(_error!);
}
